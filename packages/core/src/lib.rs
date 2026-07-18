mod ai;
mod chat;
mod http;
mod secrets;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tauri_plugin_store::StoreExt;

use crate::secrets::SecretsService;

/// 心臓周期。#17 で「トリガー毎に schedule を宣言 (最小 5m)」の設計になり、
/// framework 側の tick は下記の 2 モードだけ持つ:
/// - 通常時: 1 分 (最小粒度 5m に対して 5x のマージン)
/// - dev 時 (CHAMBERLAIN_DEV=1): 10 秒 (最小粒度も 10s に緩和される)
const TICK_INTERVAL_PROD: Duration = Duration::from_secs(60);
const TICK_INTERVAL_DEV: Duration = Duration::from_secs(10);

/// schedule 最小粒度。「秘書としての最小単位」= 5 分。
/// dev モードでは 10 秒まで緩和される。
const MIN_SCHEDULE_PROD: Duration = Duration::from_secs(5 * 60);
const MIN_SCHEDULE_DEV: Duration = Duration::from_secs(10);

const STATE_STORE_FILE: &str = "triggers-state.json";

/// state store 上の予約 namespace。framework 内部のメタ情報 (last-fire-at map 等) を
/// ここに書く。トリガーはこの ID を名乗れない (discovery で reject)。
const META_NAMESPACE: &str = "__meta__";
const META_FIRE_TIMES_KEY: &str = "fire_times";

/// CHAMBERLAIN_DEV=1 が立っているか。builder() 起動時に一度だけ evaluate する。
fn dev_mode_enabled() -> bool {
    matches!(std::env::var("CHAMBERLAIN_DEV").ok().as_deref(), Some("1"))
}

/// schedule DSL のパーサ。`5m` / `1h` / `10s` の形式のみ受け付ける。
/// 単位無し・複合単位 (`1h30m`) は非対応 (Phase 2 の cron-lite に譲る)。
fn parse_schedule(s: &str) -> Result<Duration, String> {
    let trimmed = s.trim();
    // char 単位で末尾を切り分ける (byte offset で split すると multi-byte char で panic する)
    let mut chars = trimmed.chars();
    let unit = chars
        .next_back()
        .ok_or_else(|| format!("empty schedule: '{s}'"))?;
    let num_part = chars.as_str();
    if num_part.is_empty() {
        return Err(format!("schedule missing number: '{s}'"));
    }
    let n: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid schedule number in '{s}'"))?;
    if n == 0 {
        return Err(format!("schedule must be positive: '{s}'"));
    }
    let secs = match unit {
        's' => n,
        'm' => n
            .checked_mul(60)
            .ok_or_else(|| format!("schedule overflow: '{s}'"))?,
        'h' => n
            .checked_mul(60 * 60)
            .ok_or_else(|| format!("schedule overflow: '{s}'"))?,
        other => return Err(format!("unknown schedule unit '{other}' in '{s}'")),
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_schedule_basic() {
        assert_eq!(parse_schedule("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_schedule("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_schedule("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_schedule(" 1h ").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn parse_schedule_rejects() {
        assert!(parse_schedule("").is_err());
        assert!(parse_schedule("m").is_err());
        assert!(parse_schedule("0m").is_err());
        assert!(parse_schedule("1d").is_err());
        assert!(parse_schedule("1h30m").is_err());
        assert!(parse_schedule("💾").is_err()); // multi-byte, no panic
    }
}

/// Chamberlain のフレームワーク初期化パラメータ。エージェント開発者は自分のアプリの
/// `main.rs` で本構造体を組み立て `builder()` に渡す。
pub struct ChamberlainConfig {
    /// トリガーパッケージが並ぶディレクトリ。各サブディレクトリが `manifest.json` + `index.ts`
    /// を持つ。dev では `env!("CARGO_MANIFEST_DIR")` からの相対パス、shipped 時の解決は
    /// エージェント開発者が自分で行う (詳細: docs/architecture.md 「未確定の論点」)。
    pub triggers_dir: PathBuf,
}

/// Activity event emitted whenever a trigger fires. This is the primary
/// observability surface Chamberlain exposes to its UI — see issue #6
/// ("UI as observability plane"): every trigger firing, notification, or
/// proactive action must also arrive here so the developer can watch the
/// secretary's behavior without depending on OS-level notification rendering.
#[derive(Clone, Serialize)]
struct ActivityEvent {
    ts: u64,
    source: String,
    message: String,
}

#[derive(Deserialize)]
struct NotifyPayload {
    /// 省略時は fire_trigger 側で manifest.name をデフォルトに使う
    #[serde(default)]
    title: Option<String>,
    body: String,
}

#[derive(Deserialize, Default)]
struct TickResult {
    #[serde(default)]
    notify: Option<NotifyPayload>,
    #[serde(default)]
    state: Option<serde_json::Value>,
}

#[derive(Clone, Deserialize)]
struct TriggerManifest {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    entry: String,
    /// トリガーが動作するために必要な secret 名の一覧。設定 UI に自動的に露出される。
    /// トリガーコードは `chamberlain.getSecret(name)` で任意名を読めるが、この宣言があると
    /// UI が「そのキーが未設定です」を提示できるようになる。
    #[serde(default, rename = "requiredSecrets")]
    required_secrets: Vec<String>,
    /// 発火頻度を DSL 文字列で宣言 (例: `"5m"` / `"1h"`)。必須。
    /// 意味: 「前回 fire から N 経過したら次 fire」。単位は `s` / `m` / `h`。
    /// 最小粒度は通常 `5m`、`CHAMBERLAIN_DEV=1` 時は `10s`。省略・不正値・
    /// 下限違反は discovery で reject される (詳細: #17)。
    schedule: String,
}

struct TriggerInfo {
    manifest: TriggerManifest,
    dir: PathBuf,
    paused: Arc<AtomicBool>,
    /// パース済み schedule。worker ループの発火判定と list_triggers の
    /// nextFireAt 算出の両方で使う。schedule_error があるトリガーではダミー値
    /// (Duration::ZERO)。worker は error を先に見て skip するので値は参照されない。
    schedule: Duration,
    /// schedule パース失敗 / 下限違反時のメッセージ。Some のトリガーは worker が
    /// load/tick しない。UI 側 (list_triggers) には「壊れたトリガー」として残す。
    /// 目的は「1t とタイポしたトリガーが影も形も無くなる」UX を避けること。
    /// load/instantiate error は現状 activity のみ、この gap は将来 unify したい。
    schedule_error: Option<String>,
}

type TriggersRef = Arc<Vec<TriggerInfo>>;

/// UI が受け取るトリガー一覧の要素。manifest 由来 + 現在の paused 状態 +
/// framework が持っている「次発火予定時刻」+ 起動時 discovery で見つかった構成エラー。
/// nextFireAt は「前回 fire から schedule 経過した時刻」で、初回 (まだ fire していない)
/// と error 有りは None を返す。
#[derive(Serialize)]
struct TriggerListItem {
    id: String,
    name: String,
    description: Option<String>,
    paused: bool,
    #[serde(rename = "nextFireAt")]
    next_fire_at: Option<u64>,
    /// schedule パース失敗 / 下限違反等、discovery 時点で見つかった構成エラー。
    /// Some の間は worker が load/tick しないので UI 側で「壊れてる」表示にできる。
    error: Option<String>,
}

/// UI が受け取る「要求されている secret」の集約。同じ名前を複数トリガーが要求する
/// ことがあるので、requiredBy に要求元 trigger id を列挙する形。
#[derive(Serialize)]
struct DeclaredSecretItem {
    name: String,
    #[serde(rename = "requiredBy")]
    required_by: Vec<String>,
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(windows)]
fn register_aumid(app_id: &str, display_name: &str) {
    use windows_registry::CURRENT_USER;
    if let Ok(key) = CURRENT_USER.create(format!(r"SOFTWARE\Classes\AppUserModelId\{app_id}")) {
        let _ = key.set_string("DisplayName", display_name);
    }
}

fn send_notification(app: &AppHandle, title: &str, body: &str) {
    let notification = app.notification();

    let granted = match notification.permission_state() {
        Ok(PermissionState::Granted) => true,
        Ok(_) => matches!(notification.request_permission(), Ok(PermissionState::Granted)),
        Err(_) => false,
    };

    if granted {
        let _ = notification.builder().title(title).body(body).show();
    } else {
        eprintln!("notification permission not granted");
    }
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn emit_activity(app: &AppHandle, source: &str, message: String) {
    let _ = app.emit(
        "activity",
        ActivityEvent {
            ts: now_millis(),
            source: source.into(),
            message,
        },
    );
}

/// トリガー tick が返した notify を OS 通知 + activity イベントに流す。
/// title は明示 > manifest.name の順で決まる (activity 側は body だけを流す)。
fn fire_trigger(
    app: &AppHandle,
    trigger_id: &str,
    trigger_name: &str,
    title: Option<String>,
    body: String,
) {
    let effective_title = title.unwrap_or_else(|| trigger_name.to_string());
    send_notification(app, &effective_title, &body);
    emit_activity(app, trigger_id, body);
}

fn read_trigger_state(app: &AppHandle, trigger_id: &str) -> serde_json::Value {
    match app.store(STATE_STORE_FILE) {
        Ok(store) => store
            .get(trigger_id)
            .unwrap_or_else(|| serde_json::json!({})),
        Err(e) => {
            eprintln!("failed to open state store: {e}");
            serde_json::json!({})
        }
    }
}

fn write_trigger_state(app: &AppHandle, trigger_id: &str, state: serde_json::Value) {
    match app.store(STATE_STORE_FILE) {
        Ok(store) => {
            store.set(trigger_id, state);
            if let Err(e) = store.save() {
                eprintln!("failed to persist state for {trigger_id}: {e}");
            }
        }
        Err(e) => eprintln!("failed to open state store for write: {e}"),
    }
}

/// framework が管理する「トリガー id → 最終 fire 時刻 (ms since epoch)」の map を
/// 読み出す。schedule 判定と `nextFireAt` の算出の両方で使う。まだ 1 度も fire して
/// いないトリガーはこの map に載らない。
fn read_fire_times(app: &AppHandle) -> BTreeMap<String, u64> {
    let meta = match app.store(STATE_STORE_FILE) {
        Ok(store) => store
            .get(META_NAMESPACE)
            .unwrap_or_else(|| serde_json::json!({})),
        Err(e) => {
            eprintln!("failed to open state store for meta read: {e}");
            return BTreeMap::new();
        }
    };
    meta.get(META_FIRE_TIMES_KEY)
        .and_then(|v| serde_json::from_value::<BTreeMap<String, u64>>(v.clone()).ok())
        .unwrap_or_default()
}

fn write_fire_time(app: &AppHandle, trigger_id: &str, ts: u64) {
    match app.store(STATE_STORE_FILE) {
        Ok(store) => {
            let mut meta = store
                .get(META_NAMESPACE)
                .unwrap_or_else(|| serde_json::json!({}));
            let map = meta
                .as_object_mut()
                .expect("__meta__ namespace should be a JSON object");
            let mut fire_times: BTreeMap<String, u64> = map
                .get(META_FIRE_TIMES_KEY)
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            fire_times.insert(trigger_id.to_string(), ts);
            map.insert(
                META_FIRE_TIMES_KEY.to_string(),
                serde_json::to_value(&fire_times).unwrap_or(serde_json::json!({})),
            );
            store.set(META_NAMESPACE, meta);
            if let Err(e) = store.save() {
                eprintln!("failed to persist fire_time for {trigger_id}: {e}");
            }
        }
        Err(e) => eprintln!("failed to open state store for meta write: {e}"),
    }
}

/// `triggers/*/manifest.json` を走査して有効なトリガーだけを拾う。
/// - manifest 読み取り失敗 / JSON 不正 → その 1 個をスキップ、他は続行
/// - id 重複 → 先勝ち、後発をスキップして log
/// - id が予約語 `__meta__` → reject
/// - schedule 不正 / 下限違反 → reject し activity にも `[schedule error]` で流す
/// - 実行順序を安定させるため id 昇順にソート
fn discover_triggers(app: &AppHandle, triggers_dir: &Path, dev_mode: bool) -> Vec<TriggerInfo> {
    let entries = match std::fs::read_dir(triggers_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("failed to read triggers dir {triggers_dir:?}: {e}");
            return Vec::new();
        }
    };

    let min_schedule = if dev_mode {
        MIN_SCHEDULE_DEV
    } else {
        MIN_SCHEDULE_PROD
    };

    let mut result: Vec<TriggerInfo> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("failed to read {manifest_path:?}: {e}");
                continue;
            }
        };
        let manifest: TriggerManifest = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("invalid manifest {manifest_path:?}: {e}");
                continue;
            }
        };

        if manifest.id == META_NAMESPACE {
            eprintln!(
                "trigger id '{META_NAMESPACE}' is reserved by framework, skipping {manifest_path:?}"
            );
            continue;
        }

        // schedule error はトリガーを捨てずに TriggerInfo に持たせ、UI から
        // 「壊れてる」と見えるようにする。stderr + activity にも流すが、activity は
        // discovery が .setup() 内で走る都合上 UI リスナー未接続で捨てられる可能性が
        // 高いため、list_triggers の error フィールドが実質的な観測面。
        let (schedule, schedule_error) = match parse_schedule(&manifest.schedule) {
            Ok(d) if d >= min_schedule => (d, None),
            Ok(d) => {
                let msg = format!(
                    "schedule '{}' is below minimum ({}s); minimum is {}s{}",
                    manifest.schedule,
                    d.as_secs(),
                    min_schedule.as_secs(),
                    if dev_mode {
                        " (dev)"
                    } else {
                        " — set CHAMBERLAIN_DEV=1 to lower the floor"
                    },
                );
                eprintln!("trigger '{}' schedule error: {msg}", manifest.id);
                emit_activity(app, &manifest.id, format!("[schedule error] {msg}"));
                (Duration::ZERO, Some(msg))
            }
            Err(e) => {
                eprintln!(
                    "invalid schedule for trigger '{}': {e} ({manifest_path:?})",
                    manifest.id
                );
                emit_activity(app, &manifest.id, format!("[schedule error] {e}"));
                (Duration::ZERO, Some(e))
            }
        };

        result.push(TriggerInfo {
            manifest,
            dir: path,
            paused: Arc::new(AtomicBool::new(false)),
            schedule,
            schedule_error,
        });
    }

    let mut seen = HashSet::new();
    let mut deduped: Vec<TriggerInfo> = Vec::new();
    for t in result {
        if seen.insert(t.manifest.id.clone()) {
            deduped.push(t);
        } else {
            eprintln!("duplicate trigger id '{}', skipping", t.manifest.id);
        }
    }

    deduped.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    deduped
}

/// JS ワーカー: 単一の rustyscript Runtime に N モジュールを載せ、tick 毎に順番に
/// tick() を呼ぶ。V8 の thread affinity を守るため、Runtime はこの std::thread に閉じ込め、
/// tokio 側からは mpsc で tick を送るだけ。
///
/// Per-tick per-trigger 順序: paused判定 → schedule 判定 → state読 → tick(ctx) →
/// notify → state保存 → fire_time 更新。
/// - notify が state 保存より先: プロセスクラッシュ時の "at least once" を優先 (秘書は
///   「1回多く言う > 一言忘れる」)
/// - fire_time は tick() を呼んだら常に更新 (エラー時も含む)。schedule の意味を
///   「発火試行の頻度」に統一し、エラーで毎心拍リトライになるノイズを避ける
/// - 初回 (fire_times に履歴無し) は即発火。「まだ一度も動いていない = 1 回分溜まっている」扱い
/// - catch-up: fire_time を now に上書きするので missed fires は捨てられる (#17 の合意)
fn spawn_trigger_worker(
    app: AppHandle,
    triggers: TriggersRef,
    secrets_service: SecretsService,
    tick_interval: Duration,
) {
    let (tick_tx, tick_rx) = mpsc::channel::<()>();
    let app_for_worker = app.clone();

    std::thread::spawn(move || {
        let mut runtime = match rustyscript::Runtime::new(rustyscript::RuntimeOptions {
            extensions: vec![secrets::chamberlain_ops::init()],
            ..Default::default()
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed to init JS runtime: {e}");
                return;
            }
        };

        // OpState に SecretsService を注入。op_chamberlain_get_secret がここから
        // service 名 (tauri identifier) を借りて keyring を叩く。
        runtime
            .deno_runtime()
            .op_state()
            .borrow_mut()
            .put(secrets_service);

        // 起動時に全モジュールをロード。ロード失敗したものはスキップ (他トリガーは動く)。
        // schedule_error があるトリガーはこの段階でスキップ (load しない = tick も呼ばれない)。
        // UI には list_triggers 経由で error 付きで見える。
        let mut loaded: Vec<(usize, rustyscript::ModuleHandle)> = Vec::new();
        for (idx, t) in triggers.iter().enumerate() {
            if t.schedule_error.is_some() {
                continue;
            }
            let entry_path = t.dir.join(&t.manifest.entry);
            let module = match rustyscript::Module::load(&entry_path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "failed to load trigger '{}' at {:?}: {e}",
                        t.manifest.id, entry_path
                    );
                    emit_activity(
                        &app_for_worker,
                        &t.manifest.id,
                        format!("[load error] {e}"),
                    );
                    continue;
                }
            };
            let handle = match runtime.load_module(&module) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("failed to instantiate trigger '{}': {e}", t.manifest.id);
                    emit_activity(
                        &app_for_worker,
                        &t.manifest.id,
                        format!("[instantiate error] {e}"),
                    );
                    continue;
                }
            };
            loaded.push((idx, handle));
        }

        while tick_rx.recv().is_ok() {
            let now = now_millis();
            // 心拍の頭で一括で読み、以降は per-trigger で個別に write する。
            // 同一 tick 内で先に fire したトリガーの fire_time は次 tick 以降で反映される。
            let fire_times = read_fire_times(&app_for_worker);
            for (idx, handle) in &loaded {
                let trigger = &triggers[*idx];
                if trigger.paused.load(Ordering::Relaxed) {
                    continue;
                }
                let id = &trigger.manifest.id;

                let schedule_ms = trigger.schedule.as_millis() as u64;
                let should_fire = match fire_times.get(id) {
                    None => true,
                    Some(last) => now.saturating_sub(*last) >= schedule_ms,
                };
                if !should_fire {
                    continue;
                }

                let current_state = read_trigger_state(&app_for_worker, id);
                let ctx = serde_json::json!({
                    "now": now,
                    "state": current_state,
                });
                let result: Result<Option<TickResult>, _> =
                    runtime.call_function(Some(handle), "tick", rustyscript::json_args!(ctx));
                match result {
                    Ok(Some(res)) => {
                        if let Some(notify) = res.notify {
                            fire_trigger(
                                &app_for_worker,
                                id,
                                &trigger.manifest.name,
                                notify.title,
                                notify.body,
                            );
                        }
                        if let Some(new_state) = res.state {
                            write_trigger_state(&app_for_worker, id, new_state);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("trigger '{}' tick() error: {e}", id);
                        emit_activity(&app_for_worker, id, format!("[error] {e}"));
                    }
                }
                write_fire_time(&app_for_worker, id, now);
            }
        }
    });

    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(tick_interval).await;
            if tick_tx.send(()).is_err() {
                break;
            }
        }
    });
}

#[tauri::command]
fn list_triggers(app: AppHandle, triggers: State<'_, TriggersRef>) -> Vec<TriggerListItem> {
    let fire_times = read_fire_times(&app);
    triggers
        .iter()
        .map(|t| {
            // error 付きトリガーは worker が load すらしないので next_fire_at も意味を持たない。
            // last_fire が無い (=まだ一度も動いていない) 場合も算出不能なので None。
            let next_fire_at = if t.schedule_error.is_some() {
                None
            } else {
                fire_times
                    .get(&t.manifest.id)
                    .map(|last| last.saturating_add(t.schedule.as_millis() as u64))
            };
            TriggerListItem {
                id: t.manifest.id.clone(),
                name: t.manifest.name.clone(),
                description: t.manifest.description.clone(),
                paused: t.paused.load(Ordering::Relaxed),
                next_fire_at,
                error: t.schedule_error.clone(),
            }
        })
        .collect()
}

#[tauri::command]
fn pause_trigger(id: String, triggers: State<'_, TriggersRef>) -> Result<(), String> {
    match triggers.iter().find(|t| t.manifest.id == id) {
        Some(t) => {
            t.paused.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("unknown trigger: {id}")),
    }
}

#[tauri::command]
fn resume_trigger(id: String, triggers: State<'_, TriggersRef>) -> Result<(), String> {
    match triggers.iter().find(|t| t.manifest.id == id) {
        Some(t) => {
            t.paused.store(false, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("unknown trigger: {id}")),
    }
}

/// 「今 UI が集める必要がある secret 名」を返す。framework 由来 (Chamberlain 本体が
/// 必ず要求するもの、例: anthropic_api_key) と、各トリガー manifest の
/// `requiredSecrets` を合流させる。名前の重複は 1 要素にまとめ、`required_by` に
/// 要求元 (トリガー ID or "Chamberlain") を列挙する。BTreeMap で name 昇順。
#[tauri::command]
fn list_declared_secrets(triggers: State<'_, TriggersRef>) -> Vec<DeclaredSecretItem> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();

    // framework-required (Type II 秘書 chat + 共通 chamberlain.ai.complete)
    map.entry(secrets::ANTHROPIC_API_KEY_NAME.to_string())
        .or_default()
        .push("Chamberlain".to_string());

    for t in triggers.iter() {
        for name in &t.manifest.required_secrets {
            map.entry(name.clone())
                .or_default()
                .push(t.manifest.id.clone());
        }
    }
    map.into_iter()
        .map(|(name, required_by)| DeclaredSecretItem { name, required_by })
        .collect()
}

/// Chamberlain のフレームワークが構成した Tauri Builder を返す。エージェント開発者は
/// アプリの `main.rs` で本関数を呼び、返された Builder に `.run(tauri::generate_context!())`
/// をつなげて起動する。`generate_context!` はエージェント側の `tauri.conf.json` を
/// 参照するため、必ず app crate 側で呼ぶ必要がある。
pub fn builder(config: ChamberlainConfig) -> tauri::Builder<tauri::Wry> {
    // dev 環境の逃げ道: cwd 起点で `.env` を探して load。既存の env-var は上書きしない。
    // `CHAMBERLAIN_SECRET_*` はここで拾われて `secrets::store::get` の env fallback で
    // 参照される (詳細は README「env-var fallback」)。
    let _ = dotenvy::dotenv();

    let triggers_dir = config.triggers_dir;

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            list_triggers,
            pause_trigger,
            resume_trigger,
            list_declared_secrets,
            secrets::set_secret,
            secrets::has_secret,
            secrets::delete_secret,
            chat::chat_history,
            chat::chat_send,
            chat::chat_clear,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(move |app| {
            #[cfg(windows)]
            {
                let identifier = app.config().identifier.clone();
                let display_name = app
                    .config()
                    .product_name
                    .clone()
                    .unwrap_or_else(|| identifier.clone());
                register_aumid(&identifier, &display_name);
            }

            let open_item = MenuItem::with_id(app, "open", "Open Chamberlain", true, None::<&str>)?;
            let notify_item = MenuItem::with_id(app, "notify", "Send test notification", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &notify_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "notify" => send_notification(app, "Chamberlain", "テスト通知です"),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // #17: dev モードは env-var 単独判定 (compile-time feature 化しない)。
            // dev=true 時は最小 schedule を 10s まで、心拍を 10s まで緩和する。
            let dev_mode = dev_mode_enabled();
            let tick_interval = if dev_mode {
                TICK_INTERVAL_DEV
            } else {
                TICK_INTERVAL_PROD
            };
            if dev_mode {
                eprintln!("CHAMBERLAIN_DEV=1: relaxing schedule minimum and heartbeat to 10s");
            }

            let triggers: TriggersRef =
                Arc::new(discover_triggers(app.handle(), &triggers_dir, dev_mode));
            for t in triggers.iter() {
                eprintln!(
                    "discovered trigger: {} ({}) — entry {}, schedule {}s",
                    t.manifest.id,
                    t.manifest.name,
                    t.manifest.entry,
                    t.schedule.as_secs()
                );
            }

            // Secret store の service 名として tauri.conf.json の identifier を使う。
            // Tauri state (UI commands 用) と OpState (JS op 用) の両方に持たせる。
            let secrets_service = SecretsService(app.config().identifier.clone());
            app.manage(secrets_service.clone());

            app.manage(triggers.clone());
            spawn_trigger_worker(
                app.handle().clone(),
                triggers,
                secrets_service,
                tick_interval,
            );

            Ok(())
        })
}
