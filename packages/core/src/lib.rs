mod ai;
mod chat;
mod http;
mod schedule;
mod secrets;
mod tasks;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem},
    path::BaseDirectory,
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tauri_plugin_store::StoreExt;

use crate::schedule::{parse_schedule, resolve_tz, Schedule};
use crate::secrets::SecretsService;
use crate::tasks::{
    classify_due, expand_trigger, needs_expansion, reconcile, Disposition, ExpansionState, Task,
    TaskOrigin, TaskStore, TriggerRuntimeState, TriggerSpecView,
};

/// 心臓周期。展開型スケジューラ (#26) では心拍は「due なタスクを取り出す粒度」であり、
/// 発火時刻の精度そのものではない (時刻はタスクの `scheduled_at` が持っている)。
/// - 通常時: 1 分
/// - dev 時 (CHAMBERLAIN_DEV=1): 10 秒
///
/// 手動実行 (#20) は心拍を待たずに直接この周期へ割り込む。
const TICK_INTERVAL_PROD: Duration = Duration::from_secs(60);
const TICK_INTERVAL_DEV: Duration = Duration::from_secs(10);

/// schedule 由来タスクの猶予係数。心拍 N 回分までの遅れは「心拍が拾い損ねた」として
/// 実行し、それを超えた遅れ (スリープ復帰・長時間停止) は破棄する (#26 決定事項 8)。
/// 詳細な根拠は [`crate::tasks`] のモジュール doc 参照。
///
/// 上限は DSL の最小間隔 (`@every 5m`) 未満に収めたい。猶予が最小間隔を超えると
/// 「破棄されたタスクと次のタスクが同時に見える」状態が生まれて説明しづらくなる。
/// prod で 2 分なので余裕がある。
///
/// トレードオフ: 1 つのトリガーの `tick()` が猶予より長くブロックすると、その間に due に
/// なった別タスクが次の心拍で破棄される。JS は単一スレッドで直列実行されるためこれは
/// 構造的な帰属で、猶予を伸ばして誤魔化すより「トリガーを長時間ブロックさせない」で
/// 対処すべき問題として扱う (同一心拍内のバッチは `now` を共有するので影響しない)。
const SCHEDULE_GRACE_TICKS: u32 = 2;

const STATE_STORE_FILE: &str = "triggers-state.json";

/// タスクリストと展開状態の永続先。トリガーの state (`triggers-state.json`) とは別ファイルに
/// する。`tauri-plugin-store` は `save()` でファイル全体を書くため、同居させると
/// 「トリガーが state を 1 つ書くたびに数百件のタスク配列も書き直される」write amplification が
/// 起きる (#26 ストレージ判断)。
const TASKS_STORE_FILE: &str = "tasks.json";
const TASKS_KEY: &str = "tasks";
const EXPANSION_KEY: &str = "expansion";

/// state store 上の予約 namespace。framework 内部のメタ情報を置くための予約領域。
/// トリガーはこの ID を名乗れない (discovery で reject)。
const META_NAMESPACE: &str = "__meta__";

/// 0.1.x が `__meta__` に持っていた「トリガー ID → 最終 fire 時刻」の map。
/// 0.2.0 でタスクリストが唯一の真実になったため廃止された (#26 波及範囲)。
/// 起動時に残骸を掃除するためだけに名前を残している。
const LEGACY_META_FIRE_TIMES_KEY: &str = "fire_times";

/// CHAMBERLAIN_DEV=1 が立っているか。builder() 起動時に一度だけ evaluate する。
fn dev_mode_enabled() -> bool {
    matches!(std::env::var("CHAMBERLAIN_DEV").ok().as_deref(), Some("1"))
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
    /// 発火時刻の生成規則を DSL 文字列で宣言。必須。`@` 始まりのみ
    /// (`"@hourly"` / `"@hourly :45"` / `"@every 10m"` / `"@daily 09:00"` 等)。
    ///
    /// **0.2.0 で interval 形式 (`"5m"` / `"1h"`) は廃止された** (#26 決定事項 4)。
    /// 展開器がこの規則を絶対時刻のタスクに変換する。詳細は [`crate::schedule`] 参照。
    schedule: String,
    /// IANA TZ 名 (例: `"Asia/Tokyo"`)。省略時は OS の user local を
    /// [`iana_time_zone`] で解決する。
    /// dev container の TZ 問題は `.devcontainer/devcontainer.json` の `containerEnv.TZ` で解決済み。
    #[serde(default)]
    tz: Option<String>,
}

struct TriggerInfo {
    manifest: TriggerManifest,
    dir: PathBuf,
    paused: Arc<AtomicBool>,
    /// パース済み schedule。**展開器の生成規則**として使う (実行時の発火判定には使わない)。
    /// schedule_error があるトリガーではダミー値 (`@hourly` 相当)。worker は error を先に見て
    /// 展開対象から外すので値は参照されない。
    schedule: Schedule,
    /// 解決済みの TZ。manifest.tz か user local。
    /// schedule_error があるトリガーではダミー値 (UTC)。同上、参照されない。
    tz: chrono_tz::Tz,
    /// schedule パース失敗 / tz 解決失敗時のメッセージ。Some のトリガーは worker が
    /// load/展開しない。UI 側 (list_triggers) には「壊れたトリガー」として残す。
    /// 目的は「1t とタイポしたトリガーが影も形も無くなる」UX を避けること。
    /// load/instantiate error は現状 activity のみ、この gap は将来 unify したい。
    schedule_error: Option<String>,
}

type TriggersRef = Arc<Vec<TriggerInfo>>;

/// タスクリストの共有ハンドル。worker スレッドと UI コマンドの両方が触るため
/// Mutex で包む。UI から削除・手動投入ができる (決定事項 1) 以上、worker 側の
/// in-memory コピーと UI 側の書き込みが競合しない単一の真実が必要になる。
type TaskStoreRef = Arc<Mutex<TaskStore>>;

/// 心拍への割り込みハンドル。手動実行 (#20) が次の心拍を待たずにタスクを処理させる。
/// `mpsc::Sender` は `Send` だが `!Sync` なので、Tauri state に載せるには Mutex が要る。
struct TickSignal(Mutex<mpsc::Sender<()>>);

impl TickSignal {
    /// 心拍を 1 回起こす。worker が既に落ちている場合は静かに無視する
    /// (UI 操作を失敗させても利用者にできることが無い)。
    fn poke(&self) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(());
        }
    }
}

/// UI が受け取るトリガー一覧の要素。manifest 由来 + 現在の paused 状態 +
/// 次に積まれているタスクの時刻 + 起動時 discovery で見つかった構成エラー。
///
/// `scheduleType` は 0.2.0 で削除された。interval 系統が廃止されて wall-clock のみになり
/// (#26 決定事項 4)、`nextFireAt` の意味論が分岐しなくなったため。
#[derive(Serialize)]
struct TriggerListItem {
    id: String,
    name: String,
    description: Option<String>,
    paused: bool,
    /// manifest の生の schedule 文字列。UI で「どう宣言されているか」を見せる。
    schedule: String,
    /// **タスクリスト上でこのトリガーに積まれている最も早い `scheduled_at`** (ms since epoch)。
    /// 展開型では framework が別途「次回発火予定」を計算して持つことはなく、これはタスクリストの
    /// 投影である (#26 決定事項 2)。展開前・構成エラー・全タスク削除済みの場合は null。
    #[serde(rename = "nextFireAt")]
    next_fire_at: Option<u64>,
    /// schedule パース失敗 / tz 解決失敗等、discovery 時点で見つかった構成エラー。
    /// Some の間は worker が load/展開しないので UI 側で「壊れてる」表示にできる。
    error: Option<String>,
}

/// UI が受け取るタスクリストの要素。「秘書がこれから何をするつもりか」を 1 画面で見せ、
/// かつ編集できるようにするための観測面 (#6 / #26 決定事項 1)。
#[derive(Serialize)]
struct TaskListItem {
    id: String,
    /// `"schedule"` | `"adhoc"`。UI は origin で「manifest 由来」と「手動/秘書由来」を
    /// 区別して見せる (遅延時の扱いも origin で違う)。
    origin: &'static str,
    #[serde(rename = "triggerId")]
    trigger_id: Option<String>,
    /// 実行対象トリガーの表示名。トリガーが解決できない場合は null。
    #[serde(rename = "triggerName")]
    trigger_name: Option<String>,
    #[serde(rename = "scheduledAt")]
    scheduled_at: u64,
    #[serde(rename = "createdAt")]
    created_at: u64,
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

    // permission dialog は builder().setup 側で main thread から先んじて叩いておく
    // (Issue #21 #9)。ここで request_permission() を呼ぶと worker thread から OS
    // ダイアログが上がり、そのブロッキングで tick 全体が固まる可能性がある。
    // 未取得のまま呼ばれた場合は静かに skip する (activity には出さない: 通知 UI と
    // 二重に見えて煩わしい)。
    match notification.permission_state() {
        Ok(PermissionState::Granted) => {
            let _ = notification.builder().title(title).body(body).show();
        }
        _ => {
            eprintln!("notification permission not granted; skipping");
        }
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

/// 0.1.x の `__meta__.fire_times` を掃除する (#26 波及範囲: fire_times は廃止)。
///
/// 残しておいても実害は無いが、`triggers-state.json` を開いた開発者が「どちらが真実か」で
/// 迷う。タスクリストが唯一の真実だと state ファイル上でも明示する。
fn drop_legacy_fire_times(app: &AppHandle) {
    let store = match app.store(STATE_STORE_FILE) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open state store for legacy cleanup: {e}");
            return;
        }
    };
    let Some(mut meta) = store.get(META_NAMESPACE) else {
        return;
    };
    let Some(map) = meta.as_object_mut() else {
        return;
    };
    if map.remove(LEGACY_META_FIRE_TIMES_KEY).is_none() {
        return;
    }
    eprintln!("migrating: dropped legacy __meta__.fire_times (superseded by the task list)");
    store.set(META_NAMESPACE, meta);
    if let Err(e) = store.save() {
        eprintln!("failed to persist legacy cleanup: {e}");
    }
}

/// `tasks.json` からタスクリストと展開状態を読む。
///
/// 壊れた値は「空」として扱い、起動を止めない。タスクリストは再展開で復元できる
/// (境界が失われると 1 回だけ余分に展開されるだけで、冪等性の観点でも安全側に倒れる)。
fn load_task_store(app: &AppHandle) -> TaskStore {
    let store = match app.store(TASKS_STORE_FILE) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open task store: {e}");
            return TaskStore::default();
        }
    };
    let tasks = store
        .get(TASKS_KEY)
        .and_then(|v| match serde_json::from_value::<Vec<Task>>(v) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("tasks.json: unreadable task list, starting empty: {e}");
                None
            }
        })
        .unwrap_or_default();
    let expansion = store
        .get(EXPANSION_KEY)
        .and_then(
            |v| match serde_json::from_value::<BTreeMap<String, ExpansionState>>(v) {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("tasks.json: unreadable expansion state, starting empty: {e}");
                    None
                }
            },
        )
        .unwrap_or_default();

    let mut loaded = TaskStore { tasks, expansion };
    // due 取り出しは「先頭から scheduled_at 昇順」を前提にしている。手で編集された
    // tasks.json を読んだ場合にもこの不変条件を成立させる。
    loaded.normalize();
    loaded
}

/// タスクリストと展開状態を `tasks.json` に書く。1 心拍あたり最大 1 回に抑えるのは
/// 呼び出し側 (worker) の責任。
fn save_task_store(app: &AppHandle, state: &TaskStore) {
    let store = match app.store(TASKS_STORE_FILE) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open task store for write: {e}");
            return;
        }
    };
    match serde_json::to_value(&state.tasks) {
        Ok(v) => store.set(TASKS_KEY, v),
        Err(e) => {
            eprintln!("failed to serialize task list: {e}");
            return;
        }
    }
    match serde_json::to_value(&state.expansion) {
        Ok(v) => store.set(EXPANSION_KEY, v),
        Err(e) => {
            eprintln!("failed to serialize expansion state: {e}");
            return;
        }
    }
    if let Err(e) = store.save() {
        eprintln!("failed to persist task store: {e}");
    }
}

/// `triggers/*/manifest.json` を走査して有効なトリガーだけを拾う。
/// - manifest 読み取り失敗 / JSON 不正 → その 1 個をスキップ、他は続行
/// - id 重複 → 先勝ち、後発をスキップして log
/// - id が予約語 `__meta__` → reject
/// - schedule 不正 / tz 解決失敗 → reject し activity にも `[schedule error]` で流す
/// - 実行順序を安定させるため id 昇順にソート
///
/// 発火間隔の下限チェックはここには無い。0.2.0 で interval 系統が廃止され、下限は
/// DSL パーサ側 (`@every` の許可値が 5 分以上) が構文として担保するようになった
/// (#26 決定事項 4 / 5)。これに伴い dev モードでの下限緩和も消えている
/// (秒スケールは分グリッドに原理的に載らないため、dev の反復手段は手動実行に移った)。
fn discover_triggers(app: &AppHandle, triggers_dir: &Path) -> Vec<TriggerInfo> {
    let entries = match std::fs::read_dir(triggers_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("failed to read triggers dir {triggers_dir:?}: {e}");
            return Vec::new();
        }
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
            Ok(spec) => (spec, None),
            Err(e) => {
                eprintln!(
                    "invalid schedule for trigger '{}': {e} ({manifest_path:?})",
                    manifest.id
                );
                emit_activity(app, &manifest.id, format!("[schedule error] {e}"));
                // ダミー値。schedule_error が Some の間 worker は展開しないので参照されない。
                (Schedule::Hourly { minutes: vec![0] }, Some(e))
            }
        };

        // tz 解決は schedule error があっても走らせるが、失敗した場合はエラーを追記する。
        let (tz, schedule_error) = match resolve_tz(manifest.tz.as_deref()) {
            Ok(t) => (t, schedule_error),
            Err(e) => {
                eprintln!("trigger '{}' tz error: {e}", manifest.id);
                emit_activity(app, &manifest.id, format!("[schedule error] {e}"));
                let combined = match schedule_error {
                    Some(prev) => Some(format!("{prev}; {e}")),
                    None => Some(e),
                };
                (chrono_tz::UTC, combined)
            }
        };

        result.push(TriggerInfo {
            manifest,
            dir: path,
            paused: Arc::new(AtomicBool::new(false)),
            schedule,
            tz,
            schedule_error,
        });
    }

    // sort → dedup の順にしないと、id 重複時にどちらが生き残るかが read_dir 順
    // (filesystem 順) 依存で非決定になる (Issue #21 #6)。id 昇順にしてから先勝ちなら、
    // 「同じ id が複数ある時は最初に見つかった dir が勝つ」が dir 名の辞書順で決まる。
    // ソートキーは manifest.id なので dir 名で完全に決まる訳ではないが、少なくとも
    // 同じ input からは同じ結果が出る。
    result.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));

    let mut seen = HashSet::new();
    let mut deduped: Vec<TriggerInfo> = Vec::new();
    for t in result {
        if seen.insert(t.manifest.id.clone()) {
            deduped.push(t);
        } else {
            eprintln!("duplicate trigger id '{}', skipping", t.manifest.id);
        }
    }
    deduped
}

/// タスクリストを掴む。poison は「前回の panic の残骸」として無視して中身を取り出す。
///
/// 通常なら poison は異常の伝播として尊重すべきだが、ここでは 1 度の panic 以降
/// タスクリストが永久に触れなくなる (= 秘書が二度と動かない) 方が実害が大きい。
fn lock_tasks(store: &Mutex<TaskStore>) -> std::sync::MutexGuard<'_, TaskStore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn find_trigger<'a>(triggers: &'a [TriggerInfo], id: &str) -> Option<&'a TriggerInfo> {
    triggers.iter().find(|t| t.manifest.id == id)
}

/// ms since epoch を UTC の ISO 8601 で表示する。observability メッセージ用。
///
/// ローカル時刻ではなく UTC を出すのは意図的。この repo の dev container のように
/// `/etc/localtime` と `TZ` env が食い違う環境があり (`resolve_tz` の doc 参照)、
/// `chrono::Local` は前者を見るため、トリガーの発火時刻計算とログの表示がずれる。
/// UTC 固定なら「どちらの時刻か」で迷わない。
fn fmt_utc(ms: u64) -> String {
    match i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
    {
        Some(dt) => dt.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        None => format!("<invalid ts {ms}>"),
    }
}

/// 遅延を人が読める粒度に丸める。分未満は秒、1 時間未満は分、それ以上は時間+分。
fn fmt_delay(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    format!("{}h{}m", mins / 60, mins % 60)
}

/// 閾値条件を満たしたトリガーを展開する (#26 決定事項 6)。
///
/// 展開対象は「実行可能なトリガー」だけ。構成エラーのあるトリガーや JS のロードに失敗した
/// トリガーを展開すると、実行できないタスクを積んでから due 時に破棄するだけになる。
///
/// 戻り値はタスクリスト / 境界が変化したか (呼び出し側が save の要否を判断する)。
fn expand_pending_triggers(
    app: &AppHandle,
    triggers: &[TriggerInfo],
    loaded: &BTreeMap<String, rustyscript::ModuleHandle>,
    task_store: &Mutex<TaskStore>,
    now: u64,
) -> bool {
    let mut store = lock_tasks(task_store);
    let mut dirty = false;

    for t in triggers {
        let id = &t.manifest.id;
        if t.schedule_error.is_some() || !loaded.contains_key(id) {
            continue;
        }
        // 境界が無い = reconcile 直後に消えた等の異常時。now を起点にすれば過去は生成しない。
        let boundary = store
            .expansion
            .get(id)
            .map(|s| s.expanded_until)
            .unwrap_or(now);
        if !needs_expansion(boundary, now) {
            continue;
        }

        let (generated, new_boundary) = expand_trigger(id, &t.schedule, &t.tz, boundary, now);
        let added = store.extend(generated);

        // 境界は生成 0 件でも進める。`@at` のように規則が尽きたトリガーで毎心拍
        // 展開を再試行しないため。
        store
            .expansion
            .entry(id.clone())
            .and_modify(|s| s.expanded_until = new_boundary)
            .or_insert_with(|| ExpansionState {
                expanded_until: new_boundary,
                schedule: t.manifest.schedule.clone(),
                tz: t.manifest.tz.clone(),
            });
        dirty = true;

        if added > 0 {
            emit_activity(
                app,
                id,
                format!(
                    "[expanded] {added} 件のタスクを積みました (〜{})",
                    fmt_utc(new_boundary)
                ),
            );
        }
    }
    dirty
}

/// 起動時にタスクリストを現在の manifest と突き合わせ、結果を観測面に流す。
///
/// 突き合わせには **discovery で見えている全トリガー** を渡す (ロードに失敗したものも含む)。
/// ロード失敗は一時的なこともあり、それだけで展開済み境界や積まれたタスクを破棄すると
/// 「1 回のビルド事故でスケジュールの記憶が消える」ことになる。実行可否は展開側
/// ([`expand_pending_triggers`]) と due 判定側が別途見る。
fn reconcile_at_startup(
    app: &AppHandle,
    triggers: &[TriggerInfo],
    task_store: &Mutex<TaskStore>,
    now: u64,
) {
    let views: Vec<TriggerSpecView<'_>> = triggers
        .iter()
        .map(|t| TriggerSpecView {
            id: &t.manifest.id,
            schedule: &t.manifest.schedule,
            tz: t.manifest.tz.as_deref(),
        })
        .collect();

    let mut store = lock_tasks(task_store);
    let report = reconcile(&mut store, &views, now);

    for (task_id, trigger_id) in &report.orphaned {
        eprintln!("dropped task '{task_id}': trigger '{trigger_id}' no longer exists");
        emit_activity(
            app,
            trigger_id,
            format!("[orphaned] トリガーが存在しないため予定を破棄しました ({task_id})"),
        );
    }
    for trigger_id in &report.rescheduled {
        eprintln!("trigger '{trigger_id}': schedule changed, re-expanding");
        emit_activity(
            app,
            trigger_id,
            "[rescheduled] schedule が変更されたため未実行の予定を破棄して再展開します".to_string(),
        );
    }
    save_task_store(app, &store);
}

/// JS ワーカー: 単一の rustyscript Runtime に N モジュールを載せ、心拍ごとに
/// 「due なタスクを取り出して実行する」。V8 の thread affinity を守るため、Runtime は
/// この std::thread に閉じ込め、tokio 側と UI 側からは mpsc で心拍を送るだけ。
///
/// # 心拍 1 回の流れ (#26 決定事項 2 / 6 / 8)
///
/// 1. **展開** — `expanded_until` が閾値に迫ったトリガーをホライズンまで展開する
/// 2. **due 取り出し** — `scheduled_at <= now` なタスクを昇順に取る。ここに schedule の
///    解釈は一切入らない (時刻はタスクが持っている)
/// 3. **分類** — 孤児 / pause / 遅延超過を破棄し、残りを実行する
/// 4. **後片付け** — 処理済みタスクを消して 1 回だけ永続化する
///
/// # 順序の根拠
///
/// トリガー 1 件の実行順序は state読 → tick(ctx) → notify → state保存 → タスク削除。
/// - notify が state 保存より先: プロセスクラッシュ時の "at least once" を優先
///   (秘書は「1 回多く言う > 一言忘れる」)
/// - タスク削除が最後: 同じ理由。実行中にクラッシュしたタスクはリストに残り、次回起動で
///   もう一度試される
/// - tick() がエラーを返してもタスクは消す。schedule の意味を「実行を試みる時刻」に統一し、
///   エラーで毎心拍リトライになるノイズを避ける (0.1.x の fire_time 更新方針を踏襲)
///
/// 戻り値は心拍への割り込み用 Sender。手動実行 (#20) がこれを使って次の心拍を待たずに
/// タスクを処理させる。
fn spawn_trigger_worker(
    app: AppHandle,
    triggers: TriggersRef,
    task_store: TaskStoreRef,
    secrets_service: SecretsService,
    tick_interval: Duration,
) -> mpsc::Sender<()> {
    let (tick_tx, tick_rx) = mpsc::channel::<()>();
    let app_for_worker = app.clone();
    let timer_tx = tick_tx.clone();
    // schedule 由来タスクの猶予。心拍が数回遅れたぶんは実行し、それを超えた遅れは
    // missed-fire として破棄する (#26 決定事項 8 / [`crate::tasks`] モジュール doc)。
    let schedule_grace = tick_interval * SCHEDULE_GRACE_TICKS;

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
        // schedule_error があるトリガーはこの段階でスキップ (load しない = 展開もされない)。
        // UI には list_triggers 経由で error 付きで見える。
        let mut loaded: BTreeMap<String, rustyscript::ModuleHandle> = BTreeMap::new();
        for t in triggers.iter() {
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
                    emit_activity(&app_for_worker, &t.manifest.id, format!("[load error] {e}"));
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
            loaded.insert(t.manifest.id.clone(), handle);
        }

        // 0.1.x の残骸を掃除してから、永続タスクリストを現在の manifest と突き合わせる。
        drop_legacy_fire_times(&app_for_worker);
        reconcile_at_startup(&app_for_worker, &triggers, &task_store, now_millis());

        while tick_rx.recv().is_ok() {
            let now = now_millis();

            // 1. 展開 (閾値条件)。
            let dirty =
                expand_pending_triggers(&app_for_worker, &triggers, &loaded, &task_store, now);

            // 2. due 取り出し。ロックは掴んだまま JS を回さない (list_tasks / delete_task が
            //    tick() の実行時間ぶん待たされるのを避ける)。
            let due = lock_tasks(&task_store).due(now);

            // 3. 分類と実行。
            let mut handled: Vec<String> = Vec::new();
            for task in due {
                let trigger = task
                    .trigger_id
                    .as_deref()
                    .and_then(|tid| find_trigger(&triggers, tid));
                let handle = task.trigger_id.as_deref().and_then(|tid| loaded.get(tid));
                // 実行可能なのは「discovery で見えていて、かつ JS がロード済み」のときだけ。
                // どちらかを欠く場合は classify_due に None を渡して破棄させる。
                let runtime_state = match (trigger, handle) {
                    (Some(t), Some(_)) => Some(TriggerRuntimeState {
                        paused: t.paused.load(Ordering::Relaxed),
                    }),
                    _ => None,
                };

                let source = task.trigger_id.clone().unwrap_or_else(|| "__task__".into());
                match classify_due(&task, now, runtime_state, schedule_grace) {
                    Disposition::Run { delay_ms } => {
                        // classify_due が Run を返した = trigger と handle が揃っている。
                        let (Some(t), Some(handle)) = (trigger, handle) else {
                            // trigger_id を持たないタスク (Phase 3 の自然言語タスク) は
                            // Phase 1 では実行経路が無い。積まれたら破棄して痕跡を残す。
                            emit_activity(
                                &app_for_worker,
                                &source,
                                "[unsupported] 実行対象のトリガーが無いタスクは Phase 1 では実行できません"
                                    .to_string(),
                            );
                            handled.push(task.id.clone());
                            continue;
                        };
                        let id = &t.manifest.id;
                        let current_state = read_trigger_state(&app_for_worker, id);
                        // scheduledAt / delayMs を渡すのは追加決定 11。遅延をどう伝えるかは
                        // framework が本文に前置きするのではなく、トリガー側に判断させる。
                        let ctx = serde_json::json!({
                            "now": now,
                            "state": current_state,
                            "scheduledAt": task.scheduled_at,
                            "delayMs": delay_ms,
                        });
                        let result: Result<Option<TickResult>, _> = runtime.call_function(
                            Some(handle),
                            "tick",
                            rustyscript::json_args!(ctx),
                        );
                        match result {
                            Ok(Some(res)) => {
                                if let Some(notify) = res.notify {
                                    fire_trigger(
                                        &app_for_worker,
                                        id,
                                        &t.manifest.name,
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
                                eprintln!("trigger '{id}' tick() error: {e}");
                                emit_activity(&app_for_worker, id, format!("[error] {e}"));
                            }
                        }
                    }
                    Disposition::Orphaned => {
                        // discovery では見えているのに実行できない場合と、manifest から
                        // 消えている場合を言い分ける (原因の切り分けが変わるため)。
                        let message = if trigger.is_some() {
                            "[unavailable] トリガーが実行できない状態 (構成エラー / ロード失敗) \
                             のため予定を破棄しました"
                                .to_string()
                        } else {
                            "[orphaned] トリガーが存在しないため予定を破棄しました".to_string()
                        };
                        emit_activity(&app_for_worker, &source, message);
                    }
                    Disposition::Paused => {
                        emit_activity(
                            &app_for_worker,
                            &source,
                            format!(
                                "[paused] 停止中のため {} の予定を破棄しました",
                                fmt_utc(task.scheduled_at)
                            ),
                        );
                    }
                    Disposition::SkippedLate { delay_ms } => {
                        emit_activity(
                            &app_for_worker,
                            &source,
                            format!(
                                "[skipped] {} の予定に {} 遅れているため実行しませんでした",
                                fmt_utc(task.scheduled_at),
                                fmt_delay(delay_ms)
                            ),
                        );
                    }
                    Disposition::Expired { delay_ms } => {
                        emit_activity(
                            &app_for_worker,
                            &source,
                            format!(
                                "[expired] {} の予定が猶予を超えた ({} 遅れ) ため未実行のまま破棄しました",
                                fmt_utc(task.scheduled_at),
                                fmt_delay(delay_ms)
                            ),
                        );
                    }
                }
                handled.push(task.id.clone());
            }

            // 4. 後片付け。実行後に消すことで at-least-once を保つ。
            //    展開だけがあった心拍でも境界が動いているので save は必要。
            if !handled.is_empty() || dirty {
                let mut store = lock_tasks(&task_store);
                for id in &handled {
                    store.remove(id);
                }
                save_task_store(&app_for_worker, &store);
            }
        }
    });

    tauri::async_runtime::spawn(async move {
        // 起動直後に 1 回起こす。sleep 先行だと、初回起動から最初の心拍まで (prod で 1 分)
        // 展開が走らず UI が「予定なし」を出してしまう。mpsc は buffered なので、worker が
        // モジュールロードと起動時突き合わせを終えて recv() に到達した時点で消費される。
        if timer_tx.send(()).is_err() {
            return;
        }
        loop {
            tokio::time::sleep(tick_interval).await;
            if timer_tx.send(()).is_err() {
                break;
            }
        }
    });

    tick_tx
}

/// トリガー一覧。`nextFireAt` は **タスクリストの投影** であり、framework が別に持っている
/// 「次回発火予定」ではない (#26 決定事項 2)。エンドユーザーがタスクを削除すればここも消える。
#[tauri::command]
fn list_triggers(
    triggers: State<'_, TriggersRef>,
    task_store: State<'_, TaskStoreRef>,
) -> Vec<TriggerListItem> {
    let store = lock_tasks(&task_store);
    triggers
        .iter()
        .map(|t| TriggerListItem {
            id: t.manifest.id.clone(),
            name: t.manifest.name.clone(),
            description: t.manifest.description.clone(),
            paused: t.paused.load(Ordering::Relaxed),
            schedule: t.manifest.schedule.clone(),
            next_fire_at: store.next_scheduled_for(&t.manifest.id),
            error: t.schedule_error.clone(),
        })
        .collect()
}

/// タスクリスト。「秘書がこれから何をするつもりか」の観測面 (#6 / #26 決定事項 1)。
/// pending のみが載り、終わったタスクは即座に消える。
#[tauri::command]
fn list_tasks(
    triggers: State<'_, TriggersRef>,
    task_store: State<'_, TaskStoreRef>,
) -> Vec<TaskListItem> {
    let store = lock_tasks(&task_store);
    store
        .tasks
        .iter()
        .map(|t| TaskListItem {
            id: t.id.clone(),
            origin: match t.origin {
                TaskOrigin::Schedule => "schedule",
                TaskOrigin::Adhoc => "adhoc",
            },
            trigger_id: t.trigger_id.clone(),
            trigger_name: t
                .trigger_id
                .as_deref()
                .and_then(|tid| find_trigger(&triggers, tid))
                .map(|t| t.manifest.name.clone()),
            scheduled_at: t.scheduled_at,
            created_at: t.created_at,
        })
        .collect()
}

/// タスクを 1 件削除する。
///
/// 展開済み境界 (決定事項 3) があるので、削除したタスクが次の展開パスで復活することはない。
/// 「毎日 10:00 のタスク、今日はいらないから消した」が意図どおりに効く。
#[tauri::command]
fn delete_task(
    app: AppHandle,
    id: String,
    task_store: State<'_, TaskStoreRef>,
) -> Result<(), String> {
    let mut store = lock_tasks(&task_store);
    if !store.remove(&id) {
        return Err(format!("unknown task: {id}"));
    }
    save_task_store(&app, &store);
    drop(store);
    emit_activity(
        &app,
        "__task__",
        format!("[deleted] 予定を削除しました ({id})"),
    );
    Ok(())
}

/// トリガーを今すぐ 1 回実行する (#20 を #26 Phase 1 に吸収)。
///
/// 実装は「即 due な ad-hoc タスクを 1 件積んで心拍を起こす」。#20 では
/// 「`last_fire_at` を更新しない独立動作」と決めていたが、`fire_times` 自体が廃止された
/// ため意味論を作り直す必要があった。ad-hoc タスクは展開済み境界を触らないので、
/// 手動実行が定期スケジュールを乱すことはない。
///
/// このコマンドは 0.2.0 で dev の反復手段でもある。分グリッドの DSL では秒スケールが
/// 表現できなくなり、`CHAMBERLAIN_DEV=1` による schedule 下限の緩和が消えたため
/// (#26 決定事項 5)。
#[tauri::command]
fn run_trigger_now(
    app: AppHandle,
    id: String,
    triggers: State<'_, TriggersRef>,
    task_store: State<'_, TaskStoreRef>,
    tick: State<'_, TickSignal>,
) -> Result<(), String> {
    let trigger = find_trigger(&triggers, &id).ok_or_else(|| format!("unknown trigger: {id}"))?;
    if let Some(err) = &trigger.schedule_error {
        return Err(format!("trigger '{id}' has a configuration error: {err}"));
    }

    let now = now_millis();
    {
        let mut store = lock_tasks(&task_store);
        store.insert(Task {
            id: format!("manual-{id}-{now}"),
            origin: TaskOrigin::Adhoc,
            trigger_id: Some(id.clone()),
            scheduled_at: now,
            created_at: now,
        });
        save_task_store(&app, &store);
    }
    emit_activity(&app, &id, "[manual] 手動実行を予約しました".to_string());
    // 心拍を待たせない。prod の 1 分間隔ではボタンとして成立しないため。
    tick.poke();
    Ok(())
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
///
/// トリガーの探索先は `tauri.conf.json` の `bundle.resources` で `triggers/**/*` を
/// 宣言してもらった上で、`BaseDirectory::Resource` 経由で解決する。dev では
/// `target/{debug,release}/triggers/`、shipped では platform ごとの resource dir
/// (Windows: exe と同居 / Linux: `/usr/lib/{name}/` or `${APPDIR}/usr/lib/{name}/` /
/// macOS: `{name}.app/Contents/Resources/`) を指す (詳細: #19)。
pub fn builder() -> tauri::Builder<tauri::Wry> {
    // dev 環境の逃げ道: cwd 起点で `.env` を探して load。既存の env-var は上書きしない。
    // `CHAMBERLAIN_SECRET_*` はここで拾われて `secrets::store::get` の env fallback で
    // 参照される (詳細は README「env-var fallback」)。
    let _ = dotenvy::dotenv();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            list_triggers,
            pause_trigger,
            resume_trigger,
            run_trigger_now,
            list_tasks,
            delete_task,
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

            // 通知の permission は main thread のここで 1 回だけ request しておく。
            // 以後 worker から send_notification が呼ばれても request_permission に
            // 落ちない (worker 側は permission_state だけ見る)。UX 上、初回起動時に
            // 通知ダイアログが早めに出る方が自然でもある (Issue #21 #9)。
            {
                let notification = app.notification();
                if !matches!(
                    notification.permission_state(),
                    Ok(PermissionState::Granted)
                ) {
                    let _ = notification.request_permission();
                }
            }

            let open_item = MenuItem::with_id(app, "open", "Open Chamberlain", true, None::<&str>)?;
            let notify_item =
                MenuItem::with_id(app, "notify", "Send test notification", true, None::<&str>)?;
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

            // dev モードは env-var 単独判定 (compile-time feature 化しない)。
            // 0.2.0 で緩和するのは心拍だけになった。schedule の下限は DSL パーサが構文として
            // 担保しており (`@every` は 5 分以上)、秒スケールは分グリッドに載らないため
            // (#26 決定事項 5)。dev の反復手段は手動実行 (`run_trigger_now`) に移った。
            let dev_mode = dev_mode_enabled();
            let tick_interval = if dev_mode {
                TICK_INTERVAL_DEV
            } else {
                TICK_INTERVAL_PROD
            };
            if dev_mode {
                eprintln!(
                    "CHAMBERLAIN_DEV=1: heartbeat is 10s (use the manual-run button to \
                     iterate on a trigger; schedule floors are enforced by the DSL now)"
                );
            }

            // トリガーは `bundle.resources` で `triggers/**/*` を宣言してもらった上で、
            // BaseDirectory::Resource 経由で解決する (#19)。resolve 自体は path 構築
            // だけで存在検証はしないので通常 err にはならない。万一失敗しても discover
            // は read_dir で無害な空リストを返して観測面 (list_triggers) にエラーが出る。
            let triggers_dir = match app.path().resolve("triggers", BaseDirectory::Resource) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("failed to resolve triggers resource dir: {e}");
                    PathBuf::new()
                }
            };
            let triggers: TriggersRef = Arc::new(discover_triggers(app.handle(), &triggers_dir));
            for t in triggers.iter() {
                eprintln!(
                    "discovered trigger: {} ({}) — entry {}, schedule '{}' tz={:?}",
                    t.manifest.id, t.manifest.name, t.manifest.entry, t.manifest.schedule, t.tz
                );
            }

            // Secret store の service 名として tauri.conf.json の identifier を使う。
            // Tauri state (UI commands 用) と OpState (JS op 用) の両方に持たせる。
            let secrets_service = SecretsService(app.config().identifier.clone());
            app.manage(secrets_service.clone());

            // タスクリストは worker と UI コマンドの共有状態。worker 起動前に読んでおき、
            // 起動時突き合わせ (孤児掃除 / schedule 変更検知) は worker スレッド側で行う。
            let task_store: TaskStoreRef = Arc::new(Mutex::new(load_task_store(app.handle())));

            app.manage(triggers.clone());
            app.manage(task_store.clone());
            let tick_tx = spawn_trigger_worker(
                app.handle().clone(),
                triggers,
                task_store,
                secrets_service,
                tick_interval,
            );
            app.manage(TickSignal(Mutex::new(tick_tx)));

            Ok(())
        })
}
