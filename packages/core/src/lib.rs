mod ai;
mod chat;
mod history;
mod http;
mod permissions;
mod registry;
mod schedule;
mod secrets;
mod tasks;
mod worker;

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
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tauri_plugin_store::StoreExt;

use crate::history::{origin_str, Activity, ActivityKind, HistoryStore, MAX_ROWS, RETENTION};
use crate::permissions::{parse_host_pattern, HostPattern, TriggerGrants, TriggerPermissions};
use crate::registry::{
    install_trigger, uninstall_trigger, validate_entry_path, validate_trigger_id, TriggerSource,
};
use crate::schedule::{parse_schedule, resolve_tz, Schedule};
use crate::secrets::SecretsService;
use crate::tasks::{Task, TaskOrigin, TaskStore};
use crate::worker::{
    heartbeat, lock_tasks, reconcile_at_startup, TickResult, TriggerSpec, WorkerHost, WorkerState,
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

/// トリガー 1 個の宣言ファイル。焼き込みでも実行時登録でもこの名前で探す。
const MANIFEST_FILE: &str = "manifest.json";

/// 実行時に登録されたトリガーの置き場 (`<app_data>/triggers/`) (#58)。
///
/// 焼き込み (resource dir) と**同じ形のフォルダ**をここに置く。discovery から先は
/// 出どころで区別しない — 権限の宣言も同じように強制される (#56 / #57)。
const REGISTERED_TRIGGERS_DIR: &str = "triggers";

/// タスクリストと展開状態の永続先。トリガーの state (`triggers-state.json`) とは別ファイルに
/// する。`tauri-plugin-store` は `save()` でファイル全体を書くため、同居させると
/// 「トリガーが state を 1 つ書くたびに数百件のタスク配列も書き直される」write amplification が
/// 起きる (#26 ストレージ判断)。
const TASKS_STORE_FILE: &str = "tasks.json";

/// 実行履歴 (#42)。タスクリストと違って SQLite。理由は [`crate::history`] のモジュール doc。
const HISTORY_DB_FILE: &str = "history.db";
const TASKS_KEY: &str = "tasks";
const EXPANSION_KEY: &str = "expansion";

/// state store 上の予約 namespace。framework 内部のメタ情報を置くための予約領域。
/// トリガーはこの ID を名乗れない (discovery で reject)。
const META_NAMESPACE: &str = "__meta__";

/// 廃止された「トリガー ID → 最終 fire 時刻」の map (#26 でタスクリストが唯一の真実に
/// なった)。起動時に古い state ファイルから残骸を掃除するためだけに名前を残している。
const LEGACY_META_FIRE_TIMES_KEY: &str = "fire_times";

/// CHAMBERLAIN_DEV=1 が立っているか。builder() 起動時に一度だけ evaluate する。
fn dev_mode_enabled() -> bool {
    matches!(std::env::var("CHAMBERLAIN_DEV").ok().as_deref(), Some("1"))
}

/// 観測面 (#6) に流す 1 件。トリガーの発火・通知・能動的な動作はすべてここに現れ、
/// OS 通知の描画に依存せず秘書の挙動を追えるようにする。
///
/// live emit (Tauri event) と `list_activity` (永続履歴の読み出し) は**同じ形**を返す。
/// UI が「起動前に起きたこと」と「今起きたこと」を同じ配列に混ぜられるようにするため。
#[derive(Clone, Serialize)]
struct ActivityEvent {
    /// 履歴上の行 id。UI が live emit と保存済み履歴を突き合わせる同一性に使う。
    /// 履歴 DB が開けなかった環境では null になるので、UI 側にフォールバックが要る。
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i64>,
    ts: u64,
    source: String,
    /// 種別の安定した識別子 (`"skipped"` 等)。UI がフィルタや表示の出し分けに使う。
    /// `message` の prefix はこれから組み立てられている。
    kind: String,
    /// 表示用の 1 行 (`[skipped] ...`)。
    message: String,
    /// 元になったタスクのスナップショット。展開・再展開など、タスクに紐付かない
    /// イベントでは null。
    #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    /// `"schedule"` | `"adhoc"`。履歴上で「定期の予定」と「手動・依頼」を区別する。
    #[serde(rename = "taskOrigin", skip_serializing_if = "Option::is_none")]
    task_origin: Option<String>,
    #[serde(rename = "scheduledAt", skip_serializing_if = "Option::is_none")]
    scheduled_at: Option<u64>,
}

#[derive(Clone, Deserialize)]
struct TriggerManifest {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    entry: String,
    /// トリガーが読んでよい secret 名の一覧 (#56)。設定 UI に自動的に露出される。
    ///
    /// **これは実行時の権限である。** 宣言していない名前を `chamberlain.getSecret(name)`
    /// に渡すと `null` が返り、拒否が `[denied]` として観測面に残る。判断は
    /// [`crate::permissions`] にある。
    #[serde(default, rename = "requiredSecrets")]
    required_secrets: Vec<String>,
    /// このトリガーが `chamberlain.http.fetch` で出てよい宛先ホストの一覧 (#57)。
    ///
    /// `"api.github.com"` (完全一致) と `"*.example.com"` (サブドメインのみ) を書ける。
    /// 宣言外のホストへの fetch は失敗し、`[denied]` として観測面に残る。リダイレクトも
    /// ホップごとに照合される。**書かなければ一切ネットワークに出られない**。
    /// 検証は discovery で行い、壊れた宣言はトリガーごと実行対象から外す。
    #[serde(default, rename = "allowedHosts")]
    allowed_hosts: Vec<String>,
    /// 発火時刻の生成規則を DSL 文字列で宣言。必須。`@` 始まりのみ
    /// (`"@hourly"` / `"@hourly :45"` / `"@every 10m"` / `"@daily 09:00"` 等)。
    /// 展開器がこの規則を絶対時刻のタスクに変換する。詳細は [`crate::schedule`] 参照。
    schedule: String,
    /// IANA TZ 名 (例: `"Asia/Tokyo"`)。省略時は OS の user local を解決する
    /// ([`crate::schedule::resolve_tz`])。
    #[serde(default)]
    tz: Option<String>,
}

struct TriggerInfo {
    manifest: TriggerManifest,
    dir: PathBuf,
    /// 焼き込みか、実行時登録か (#58)。UI に出すほか、「解除できるか」の判断に使う。
    source: TriggerSource,
    paused: Arc<AtomicBool>,
    /// 実行時に解除された (#58)。**解除は再起動を待たずに効く。**
    ///
    /// discovery は起動時 1 回で確定するので、解除したトリガーはこのプロセスの
    /// in-memory の一覧には残り続ける。フラグを立てて心拍・UI・手動実行の全経路から
    /// 外すことで、「外したのにまだ動く」を再起動まで引きずらない。JS モジュールは
    /// ロードされたままだが、そこへ到達する経路が無くなる。
    unregistered: Arc<AtomicBool>,
    /// パース済み schedule。**展開器の生成規則**として使う (実行時の発火判定には使わない)。
    /// config_error があるトリガーではダミー値 (`@hourly` 相当)。worker は error を先に見て
    /// 展開対象から外すので値は参照されない。
    schedule: Schedule,
    /// 解決済みの TZ。manifest.tz か user local。
    /// config_error があるトリガーではダミー値 (UTC)。同上、参照されない。
    tz: chrono_tz::Tz,
    /// manifest の構成エラー (schedule パース失敗 / tz 解決失敗 / `allowedHosts` の
    /// 書式不正)。Some のトリガーは worker が load/展開しないが、UI (list_triggers) には
    /// 「壊れたトリガー」として残す — タイポしたトリガーが影も形も無くなる UX を避ける。
    /// load/instantiate error は現状 activity のみで、この gap は将来 unify したい。
    config_error: Option<String>,
    /// 検証済みの `allowedHosts` (#57)。`config_error` があるトリガーでは空。
    hosts: Vec<HostPattern>,
}

type TriggersRef = Arc<Vec<TriggerInfo>>;

/// タスクリストの共有ハンドル。worker スレッドと UI コマンドの両方が触るため
/// Mutex で包む。UI から削除・手動投入ができる (決定事項 1) 以上、worker 側の
/// in-memory コピーと UI 側の書き込みが競合しない単一の真実が必要になる。
type TaskStoreRef = Arc<Mutex<TaskStore>>;

/// 履歴の共有ハンドル。worker スレッド (書き手) と UI コマンド (読み手 / 書き手) の
/// 両方が触る。`rusqlite::Connection` は `Send` だが `!Sync` なので Mutex で包む。
///
/// `Option` なのは **DB が開けなくても秘書は動くべき**だから。開けなかった環境では
/// 履歴が残らないだけで、live emit も心拍もそのまま動く。
type HistoryRef = Arc<Mutex<Option<HistoryStore>>>;

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
    /// schedule パース失敗 / tz 解決失敗 / `allowedHosts` 不正等、discovery 時点で
    /// 見つかった構成エラー。Some の間は worker が load/展開しないので UI 側で
    /// 「壊れてる」表示にできる。
    error: Option<String>,
    /// このトリガーが要求している権限 (#56 / #57)。**宣言をそのまま見せるためのもの。**
    ///
    /// エンドユーザーが「このトリガーは何を読み、どこへ出るのか」を実行前に読めることが、
    /// 実行時登録 (#55) の同意画面の中身になる。強制力は core が持っているので、ここに
    /// 出る文字列は「見せかけ」ではなく実際の制限そのものである。
    #[serde(rename = "requiredSecrets")]
    required_secrets: Vec<String>,
    #[serde(rename = "allowedHosts")]
    allowed_hosts: Vec<String>,
    /// `"bundled"` (アプリに焼き込まれた) | `"registered"` (実行時に登録された) (#58)。
    ///
    /// エンドユーザーが外せるのは後者だけ。「アプリの形」と「秘書にさせる仕事」の
    /// 線引きがここに現れる。
    source: &'static str,
}

/// 登録しようとしているトリガーの下見 (#58)。**同意画面に出す内容そのもの。**
///
/// `requiredSecrets` / `allowedHosts` は core が実際に強制している宣言なので (#56 / #57)、
/// ここに出た以上のことはそのトリガーにはできない。宣言が強制力を持つ状態で初めて、
/// 入れる前に見せる意味がある。
#[derive(Clone, Debug, Serialize)]
struct TriggerCandidate {
    /// 選ばれたフォルダの絶対パス。UI はこれをそのまま `register_trigger` に返す。
    path: String,
    id: String,
    name: String,
    description: Option<String>,
    schedule: String,
    tz: Option<String>,
    #[serde(rename = "requiredSecrets")]
    required_secrets: Vec<String>,
    #[serde(rename = "allowedHosts")]
    allowed_hosts: Vec<String>,
    /// 同じ id が既にある場合の相手 (`"bundled"` | `"registered"`)。
    ///
    /// `"bundled"` は登録を拒否する — アプリに同梱された「そのアプリらしさ」を後から
    /// 乗っ取られないため。`"registered"` は置き換え (配布物の更新) として通す。
    conflict: Option<&'static str>,
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

/// 履歴を掴む。poison の扱いは [`crate::worker::lock_tasks`] と同じ理由で無視する。
fn lock_history(
    history: &Mutex<Option<HistoryStore>>,
) -> std::sync::MutexGuard<'_, Option<HistoryStore>> {
    history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 観測面に 1 件流す。**live emit と永続化の両方**を行う (#42)。
///
/// 永続化に失敗しても emit は続ける。履歴が欠けるより、秘書が今何をしているかが
/// 見えなくなる方が実害が大きい。
fn record_activity(app: &AppHandle, history: &HistoryRef, activity: &Activity) {
    let ts = now_millis();
    let id = match lock_history(history).as_ref() {
        Some(store) => match store.append(ts, activity) {
            Ok(id) => Some(id),
            Err(e) => {
                eprintln!("failed to persist activity: {e}");
                None
            }
        },
        None => None,
    };
    let _ = app.emit(
        "activity",
        ActivityEvent {
            id,
            ts,
            source: activity.source.clone(),
            kind: activity.kind.as_str().to_string(),
            message: activity.display(),
            task_id: activity.task.as_ref().map(|t| t.id.clone()),
            task_origin: activity
                .task
                .as_ref()
                .map(|t| origin_str(t.origin).to_string()),
            scheduled_at: activity.task.as_ref().map(|t| t.scheduled_at),
        },
    );
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

/// トリガーの永続 state を捨てる (#58 の解除)。
///
/// 残しておくと、同じ id を入れ直したときに前の住人の state を引き継ぐ。外したものの
/// 痕跡が別のトリガーの初期状態になるのは説明できないので、解除と一緒に消す。
fn remove_trigger_state(app: &AppHandle, trigger_id: &str) {
    match app.store(STATE_STORE_FILE) {
        Ok(store) => {
            if store.delete(trigger_id) {
                if let Err(e) = store.save() {
                    eprintln!("failed to persist state removal for {trigger_id}: {e}");
                }
            }
        }
        Err(e) => eprintln!("failed to open state store for removal: {e}"),
    }
}

/// 実行時登録の置き場 (`<app_data>/triggers/`) を解決して作る (#58)。
///
/// 起動時にも作る。**空でも存在していること**に意味があり、「フォルダを開いて直接置く」
/// 経路 (#55 の受け取り口の最小形) がそこにあるだけで成立する。
fn registered_triggers_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir を解決できません: {e}"))?
        .join(REGISTERED_TRIGGERS_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("{} を作成できません: {e}", dir.display()))?;
    Ok(dir)
}

/// 古い state ファイルに残った `__meta__.fire_times` を掃除する。
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

/// `tasks.json` からタスクリストと展開状態を読む。JSON の解釈そのものは
/// [`TaskStore::from_stored`] にあり (テスト可能)、ここは store プラグインとの接続だけ。
fn load_task_store(app: &AppHandle) -> TaskStore {
    let store = match app.store(TASKS_STORE_FILE) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open task store: {e}");
            return TaskStore::default();
        }
    };
    let (loaded, warnings) = TaskStore::from_stored(store.get(TASKS_KEY), store.get(EXPANSION_KEY));
    for w in warnings {
        eprintln!("{w}");
    }
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
    let (tasks, expansion) = match state.to_stored() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    store.set(TASKS_KEY, tasks);
    store.set(EXPANSION_KEY, expansion);
    if let Err(e) = store.save() {
        eprintln!("failed to persist task store: {e}");
    }
}

/// 履歴 DB を `<app_data>/history.db` に開く。
///
/// 失敗しても `None` を返すだけで起動は止めない。履歴が残らないのは観測面の劣化だが、
/// それで秘書が動かなくなる方が実害が大きい (live emit は引き続き動く)。
fn open_history(app: &AppHandle) -> Option<HistoryStore> {
    let dir = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to resolve app data dir for history: {e}");
            return None;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create app data dir for history: {e}");
        return None;
    }
    let path = dir.join(HISTORY_DB_FILE);
    match HistoryStore::open(&path) {
        Ok(store) => Some(store),
        Err(e) => {
            eprintln!("failed to open activity history at {path:?}: {e}");
            None
        }
    }
}

/// manifest の意味を検証した結果。
///
/// **焼き込みと実行時登録で同じものを使う** (#55)。例外を作ると「焼き込みなら何でもできる」
/// 抜け道になり、焼き込みを偽装する経路が価値を持つ。
struct ValidatedManifest {
    /// パース済み schedule。エラーがあるときはダミー (worker が参照しない)。
    schedule: Schedule,
    /// 解決済み TZ。同上。
    tz: chrono_tz::Tz,
    /// 検証済みの `allowedHosts` (#57)。エラーがあるときは空。
    hosts: Vec<HostPattern>,
    /// 見つかった構成エラー。1 件ずつ観測面に流せるよう配列で持つ。
    errors: Vec<String>,
}

impl ValidatedManifest {
    /// UI に見せる 1 本の文字列。複数壊れているときは `; ` で連ねる。
    fn config_error(&self) -> Option<String> {
        (!self.errors.is_empty()).then(|| self.errors.join("; "))
    }
}

/// manifest を検証する。**壊れた宣言はトリガーごと実行対象から外す** (#57)。
///
/// 悪い書き方を黙って捨てて残りで動かすと、登録時の同意画面 (#58) に出す文字列と実際の
/// 制限がずれる。宣言が強制力を持たない状態で同意だけ取るのはシアターなので、宣言が
/// 読めないうちは走らせない方を採る。
fn validate_manifest(manifest: &TriggerManifest) -> ValidatedManifest {
    let mut errors: Vec<String> = Vec::new();

    // ダミー値を置くのは、config_error がある間 worker が展開しないため (参照されない)。
    let schedule = match parse_schedule(&manifest.schedule) {
        Ok(spec) => spec,
        Err(e) => {
            errors.push(e);
            Schedule::Hourly { minutes: vec![0] }
        }
    };
    // tz 解決は schedule のパースに失敗していても走らせ、両方壊れていれば連ねて出す。
    let tz = match resolve_tz(manifest.tz.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            errors.push(e);
            chrono_tz::UTC
        }
    };
    let hosts = match manifest
        .allowed_hosts
        .iter()
        .map(|h| parse_host_pattern(h))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(hosts) => hosts,
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };
    // entry がトリガーのディレクトリ内を指していること (#58)。V8 に外のファイルを
    // 読ませる経路になるので、焼き込みにも同じ検証をかける。
    if let Err(e) = validate_entry_path(&manifest.entry) {
        errors.push(e);
    }

    ValidatedManifest {
        schedule,
        tz,
        hosts,
        errors,
    }
}

/// 1 つのディレクトリを走査して有効なトリガーだけを拾う。
/// - manifest 読み取り失敗 / JSON 不正 → その 1 個をスキップ、他は続行
/// - id 重複 → 先勝ち、後発をスキップして log
/// - id が予約語 `__meta__` → reject
/// - schedule 不正 / tz 解決失敗 / 宣言不正 → reject し activity にも `[config error]` で流す
/// - 実行順序を安定させるため id 昇順にソート
///
/// 発火間隔の下限チェックはここには無い。下限は DSL パーサ側 (`@every` の許可値が
/// 5 分以上) が構文として担保する (#26 決定事項 5)。
fn discover_triggers(
    app: &AppHandle,
    history: &HistoryRef,
    triggers_dir: &Path,
    source: TriggerSource,
) -> Vec<TriggerInfo> {
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
        // ドット始まりは framework の作業領域 (登録中の `.staging-*`)。トリガーとして
        // 拾うと、コピー途中の半端なフォルダが本物と同じ id で競合しうる。
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let manifest_path = path.join(MANIFEST_FILE);
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

        // 構成エラーはトリガーを捨てずに TriggerInfo に持たせ、UI から「壊れてる」と
        // 見えるようにする。stderr + activity にも流す (discovery は .setup() 内で走るので
        // UI リスナーには届かないが、履歴に残るので起動後に list_activity で読める)。
        let validated = validate_manifest(&manifest);
        for e in &validated.errors {
            eprintln!(
                "trigger '{}' config error: {e} ({manifest_path:?})",
                manifest.id
            );
            record_activity(
                app,
                history,
                &Activity::new(&manifest.id, ActivityKind::ConfigError, e.clone()),
            );
        }

        let config_error = validated.config_error();
        result.push(TriggerInfo {
            manifest,
            dir: path,
            source,
            paused: Arc::new(AtomicBool::new(false)),
            unregistered: Arc::new(AtomicBool::new(false)),
            schedule: validated.schedule,
            tz: validated.tz,
            config_error,
            hosts: validated.hosts,
        });
    }

    // sort → dedup の順にしないと、id 重複時にどちらが生き残るかが read_dir 順
    // (filesystem 順) 依存で非決定になる (Issue #21 #6)。id 昇順にしてから先勝ちにすれば、
    // 少なくとも同じ input からは同じ結果が出る。
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

/// 2 つのソースを走査して 1 本のトリガー一覧にする (#58)。
///
/// - 焼き込み (resource dir) — エージェント開発者が同梱したもの
/// - 実行時登録 (`<app_data>/triggers/`) — エンドユーザーが後から入れたもの
///
/// **id が衝突したら焼き込み側を優先する。** アプリに同梱された「そのアプリらしさ」を
/// 後から乗っ取られないため。登録の入口 ([`register_trigger`]) が同じ衝突を先に弾くので、
/// ここに落ちてくるのは「アプリの更新で同じ id の焼き込みが後から増えた」場合が主になる。
/// 黙って消すと理由が分からないので観測面に残す。
fn discover_all(
    app: &AppHandle,
    history: &HistoryRef,
    bundled_dir: &Path,
    registered_dir: Option<&Path>,
) -> Vec<TriggerInfo> {
    let bundled = discover_triggers(app, history, bundled_dir, TriggerSource::Bundled);
    let Some(registered_dir) = registered_dir else {
        return bundled;
    };
    let registered = discover_triggers(app, history, registered_dir, TriggerSource::Registered);

    let (merged, shadowed) = merge_sources(bundled, registered);
    for t in shadowed {
        let message = format!(
            "同じ id の同梱トリガーがあるため、登録された方を無視しました ({})",
            t.dir.display()
        );
        eprintln!("trigger '{}': {message}", t.manifest.id);
        record_activity(
            app,
            history,
            &Activity::new(&t.manifest.id, ActivityKind::ConfigError, message),
        );
    }
    merged
}

/// 2 ソースの取捨 (#58)。戻り値は (採用したもの, 焼き込みに負けたもの)。
///
/// **焼き込みが勝つ。** discovery より後は出どころで区別しないので、ここが「同梱された
/// トリガーは後から差し替えられない」を担保する唯一の場所になる。
fn merge_sources(
    bundled: Vec<TriggerInfo>,
    registered: Vec<TriggerInfo>,
) -> (Vec<TriggerInfo>, Vec<TriggerInfo>) {
    let bundled_ids: HashSet<String> = bundled.iter().map(|t| t.manifest.id.clone()).collect();
    let mut merged = bundled;
    let mut shadowed = Vec::new();

    for t in registered {
        if bundled_ids.contains(&t.manifest.id) {
            shadowed.push(t);
        } else {
            merged.push(t);
        }
    }
    merged.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    (merged, shadowed)
}

/// 解除されていないトリガーだけを見る (#58)。
///
/// 解除は再起動を待たずに効くので、「今このプロセスが仕事として持っているもの」は
/// このフィルタを通った側である。心拍・UI・手動実行はすべてここを経由する。
fn active_triggers(triggers: &[TriggerInfo]) -> impl Iterator<Item = &TriggerInfo> {
    triggers
        .iter()
        .filter(|t| !t.unregistered.load(Ordering::Relaxed))
}

fn find_trigger<'a>(triggers: &'a [TriggerInfo], id: &str) -> Option<&'a TriggerInfo> {
    active_triggers(triggers).find(|t| t.manifest.id == id)
}

/// 心拍から見えるトリガーの写像を作る。`paused` は心拍ごとに読み直す値なので、
/// この関数も心拍ごとに呼ぶ (#46: 配線層のテストのために worker は [`TriggerSpec`] しか
/// 知らない)。
///
/// `runnable` は「構成エラーが無く、かつ JS がロード済み」。どちらを欠いても展開されず、
/// 既に積まれたタスクは due 時に `[unavailable]` で破棄される。
///
/// 解除されたトリガー (#58) はここに現れない。心拍から見れば「manifest から消えた」のと
/// 同じ扱いになり、残っている予定は孤児として片付く。
fn build_specs(triggers: &[TriggerInfo], host: &TauriHost) -> Vec<TriggerSpec> {
    active_triggers(triggers)
        .map(|t| TriggerSpec {
            id: t.manifest.id.clone(),
            name: t.manifest.name.clone(),
            schedule_raw: t.manifest.schedule.clone(),
            tz_raw: t.manifest.tz.clone(),
            schedule: t.schedule.clone(),
            tz: t.tz,
            paused: t.paused.load(Ordering::Relaxed),
            runnable: t.config_error.is_none() && host.is_loaded(&t.manifest.id),
        })
        .collect()
}

/// manifest の宣言を実行時の権限の形に写す (#56)。
///
/// **焼き込みか実行時登録かで区別しない。** 例外を作ると「焼き込みなら何でもできる」抜け道に
/// なり、#55 で焼き込みを偽装する経路が価値を持ってしまう。詳細は [`crate::permissions`]。
fn build_grants(triggers: &[TriggerInfo]) -> BTreeMap<String, TriggerGrants> {
    triggers
        .iter()
        .map(|t| {
            (
                t.manifest.id.clone(),
                TriggerGrants {
                    secrets: t.manifest.required_secrets.iter().cloned().collect(),
                    hosts: t.hosts.clone(),
                },
            )
        })
        .collect()
}

/// 本番の [`WorkerHost`]。worker スレッドが所有する副作用の実体をひとまとめにする。
///
/// `rustyscript::Runtime` は V8 の thread affinity を持つのでこの構造体ごと worker
/// スレッドに閉じ込める (`AppHandle` は `Send + Sync` なので問題ない)。
struct TauriHost {
    app: AppHandle,
    runtime: rustyscript::Runtime,
    /// ロードに成功したトリガーのモジュール。起動時に 1 回作られ、以後変わらない。
    loaded: BTreeMap<String, rustyscript::ModuleHandle>,
    history: HistoryRef,
}

impl TauriHost {
    fn is_loaded(&self, trigger_id: &str) -> bool {
        self.loaded.contains_key(trigger_id)
    }

    /// `OpState` に載せた [`TriggerPermissions`] を触る。
    ///
    /// **JS が動いていない間だけ呼べる。** `op_state()` の `RefCell` は op の実行中に
    /// 借りられているので、JS 実行中に呼ぶと panic する。呼び出し箇所を
    /// [`Self::run_js`] の前後に限っているのはこのため。
    fn with_permissions<R>(&mut self, f: impl FnOnce(&mut TriggerPermissions) -> R) -> R {
        let op_state = self.runtime.deno_runtime().op_state();
        let mut op_state = op_state.borrow_mut();
        f(op_state.borrow_mut::<TriggerPermissions>())
    }

    /// トリガーの JS を「そのトリガーとして」動かす (#56)。**JS を動かす経路は必ずここを
    /// 通す** — 通さない経路は実行文脈の外になり、その JS からは何も読めなくなる。
    ///
    /// 呼び出し元の識別を JS 側に名乗らせると自己申告になるので、Rust 側が実行の前後で
    /// 現在のトリガーを立てる。op はこれを見て manifest の宣言と突き合わせる。
    ///
    /// op は自分では activity を書けない (`AppHandle` を持たない) ので、溜まったものを
    /// ここで回収して観測面に流す (`[denied]` / `[ai]`)。帰属先を持たない記録
    /// (= run_js を通らない経路から来たもの) は、今動かしているトリガーのせいにせず
    /// framework 側 (`__meta__`) に付ける。誤った帰属は観測面としては無いより悪い。
    fn run_js<R>(&mut self, trigger_id: &str, f: impl FnOnce(&mut Self) -> R) -> R {
        self.with_permissions(|p| p.enter(trigger_id));
        let result = f(self);
        for op_activity in self.with_permissions(|p| p.leave()) {
            let source = op_activity
                .trigger_id
                .clone()
                .unwrap_or_else(|| META_NAMESPACE.into());
            let message = op_activity.display();
            eprintln!("[{}] {source}: {message}", op_activity.kind.as_str());
            self.activity(Activity::new(source, op_activity.kind, message));
        }
        result
    }
}

impl WorkerHost for TauriHost {
    fn read_state(&mut self, trigger_id: &str) -> serde_json::Value {
        read_trigger_state(&self.app, trigger_id)
    }

    fn write_state(&mut self, trigger_id: &str, state: serde_json::Value) {
        write_trigger_state(&self.app, trigger_id, state);
    }

    fn call_tick(
        &mut self,
        trigger_id: &str,
        ctx: serde_json::Value,
    ) -> Result<Option<TickResult>, String> {
        // 未ロードの判定は run_js の外で済ませる。中で早期 return すると
        // enter したまま leave されない経路ができる。
        if !self.is_loaded(trigger_id) {
            return Err(format!("trigger '{trigger_id}' is not loaded"));
        }
        self.run_js(trigger_id, |host| {
            // runtime (可変借用) と loaded (不変借用) を同時に使うためフィールドを分解する。
            let Self {
                runtime, loaded, ..
            } = host;
            // 直前の is_loaded で存在を確認済み。
            let handle = &loaded[trigger_id];
            runtime
                .call_function(Some(handle), "tick", rustyscript::json_args!(ctx))
                .map_err(|e| e.to_string())
        })
    }

    fn notify(&mut self, title: &str, body: &str) {
        send_notification(&self.app, title, body);
    }

    fn activity(&mut self, activity: Activity) {
        record_activity(&self.app, &self.history, &activity);
    }

    fn sweep_history(&mut self, now: u64) -> usize {
        match lock_history(&self.history).as_ref() {
            Some(store) => match store.sweep(now, RETENTION, MAX_ROWS) {
                Ok(removed) => removed,
                Err(e) => {
                    eprintln!("failed to sweep activity history: {e}");
                    0
                }
            },
            None => 0,
        }
    }

    fn save_tasks(&mut self, store: &TaskStore) {
        save_task_store(&self.app, store);
    }
}

/// JS ワーカー: 単一の rustyscript Runtime に N モジュールを載せ、心拍ごとに
/// 「due なタスクを取り出して実行する」。V8 の thread affinity を守るため、Runtime は
/// この std::thread に閉じ込め、tokio 側と UI 側からは mpsc で心拍を送るだけ。
///
/// 心拍 1 回の中身は [`crate::worker::heartbeat`] にある。ここが持つのは
/// 「Runtime を立てて、モジュールを載せて、時計を送る」までで、副作用の順序や
/// 分類の判断は [`TauriHost`] の裏に押し出してある (#46)。
///
/// 戻り値は心拍への割り込み用 Sender。手動実行 (#20) がこれを使って次の心拍を待たずに
/// タスクを処理させる。
fn spawn_trigger_worker(
    app: AppHandle,
    triggers: TriggersRef,
    task_store: TaskStoreRef,
    history: HistoryRef,
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
        // service 名 (tauri identifier) を借りて keyring を叩く。併せて、extension が
        // 載せた既定の TriggerPermissions を manifest 由来の宣言で差し替える (#56)。
        // op から見える権限の判断材料はこれだけで、JS 側から差し替える手段は無い。
        {
            let op_state = runtime.deno_runtime().op_state();
            let mut op_state = op_state.borrow_mut();
            op_state.put(secrets_service);
            op_state.put(TriggerPermissions::new(build_grants(&triggers)));
        }

        let mut host = TauriHost {
            app: app_for_worker,
            runtime,
            loaded: BTreeMap::new(),
            history,
        };

        // 起動時に全モジュールをロード。ロード失敗したものはスキップ (他トリガーは動く)。
        // config_error があるトリガーはこの段階でスキップ (load しない = 展開もされない)。
        // UI には list_triggers 経由で error 付きで見える。
        for t in triggers.iter() {
            if t.config_error.is_some() {
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
                    host.activity(Activity::new(
                        &t.manifest.id,
                        ActivityKind::LoadError,
                        e.to_string(),
                    ));
                    continue;
                }
            };
            // load_module はモジュールのトップレベルを実行する = JS が動く。宣言を正しく
            // 書いたトリガーが初期化時に自分の secret を読めるよう、ここも実行文脈に含める。
            // 含めなくても既定拒否で安全側には倒れるが、正しいトリガーが理由の分かりにくい
            // [denied] を踏むことになる。
            let loaded = host.run_js(&t.manifest.id, |host| host.runtime.load_module(&module));
            let handle = match loaded {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("failed to instantiate trigger '{}': {e}", t.manifest.id);
                    host.activity(Activity::new(
                        &t.manifest.id,
                        ActivityKind::InstantiateError,
                        e.to_string(),
                    ));
                    continue;
                }
            };
            host.loaded.insert(t.manifest.id.clone(), handle);
        }

        // 古い state の残骸を掃除してから、永続タスクリストを現在の manifest と突き合わせる。
        drop_legacy_fire_times(&host.app);
        let specs = build_specs(&triggers, &host);
        reconcile_at_startup(&mut host, &specs, &task_store, now_millis());

        let mut state = WorkerState::default();
        while tick_rx.recv().is_ok() {
            // paused はここで読み直す (UI から心拍の合間に切り替わる)。
            let specs = build_specs(&triggers, &host);
            heartbeat(
                &mut host,
                &specs,
                &task_store,
                &mut state,
                now_millis(),
                schedule_grace,
            );
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

/// 保存済みの履歴を新しい順に返す (#42)。
///
/// **このコマンドが起動時イベントの gap を閉じる。** worker は `.setup()` 内で動き出すため、
/// `[config error]` / `[expanded]` / `[rescheduled]` / `[orphaned]` は webview のリスナーが
/// 繋がる前に emit される。UI は起動後にこれを読めばよい。
#[tauri::command]
fn list_activity(limit: Option<usize>, history: State<'_, HistoryRef>) -> Vec<ActivityEvent> {
    // 上限を設けるのは、UI が誤って全期間を要求しても DB とメモリを踏み抜かないため。
    let limit = limit.unwrap_or(200).min(1000);
    let Some(store) = lock_history(&history).as_ref().map(|s| s.recent(limit)) else {
        return Vec::new();
    };
    match store {
        Ok(rows) => rows
            .into_iter()
            .map(|r| ActivityEvent {
                id: Some(r.id),
                ts: r.ts,
                message: r.display(),
                source: r.source,
                kind: r.kind,
                task_id: r.task_id,
                task_origin: r.task_origin,
                scheduled_at: r.scheduled_at,
            })
            .collect(),
        Err(e) => {
            eprintln!("failed to read activity history: {e}");
            Vec::new()
        }
    }
}

/// トリガー一覧。`nextFireAt` は **タスクリストの投影** であり、framework が別に持っている
/// 「次回発火予定」ではない (#26 決定事項 2)。エンドユーザーがタスクを削除すればここも消える。
#[tauri::command]
fn list_triggers(
    triggers: State<'_, TriggersRef>,
    task_store: State<'_, TaskStoreRef>,
) -> Vec<TriggerListItem> {
    let store = lock_tasks(&task_store);
    active_triggers(&triggers)
        .map(|t| TriggerListItem {
            id: t.manifest.id.clone(),
            name: t.manifest.name.clone(),
            description: t.manifest.description.clone(),
            paused: t.paused.load(Ordering::Relaxed),
            schedule: t.manifest.schedule.clone(),
            next_fire_at: store.next_scheduled_for(&t.manifest.id),
            error: t.config_error.clone(),
            required_secrets: t.manifest.required_secrets.clone(),
            // 検証済みのパターンから組み立て直す。manifest の生文字列をそのまま出すと、
            // 大文字や前後の空白の差で「UI に見えている文字列」と「実際に効く宣言」が
            // ずれる。ここで見せるものは強制力を持つ側と同一でなければならない。
            allowed_hosts: t.hosts.iter().map(|h| h.as_declared()).collect(),
            source: t.source.as_str(),
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
    history: State<'_, HistoryRef>,
) -> Result<(), String> {
    let mut store = lock_tasks(&task_store);
    if !store.remove(&id) {
        return Err(format!("unknown task: {id}"));
    }
    save_task_store(&app, &store);
    drop(store);
    record_activity(
        &app,
        &history,
        &Activity::new(
            "__task__",
            ActivityKind::Deleted,
            format!("予定を削除しました ({id})"),
        ),
    );
    Ok(())
}

/// トリガーを今すぐ 1 回実行する (#20)。
///
/// 実装は「即 due な ad-hoc タスクを 1 件積んで心拍を起こす」。ad-hoc タスクは展開済み
/// 境界を触らないので、手動実行が定期スケジュールを乱すことはない。
///
/// dev の反復手段でもある。分グリッドの DSL では秒スケールを表現できないので、
/// トリガーを何度も試したいときはこれを使う (#26 決定事項 5)。
#[tauri::command]
fn run_trigger_now(
    app: AppHandle,
    id: String,
    triggers: State<'_, TriggersRef>,
    task_store: State<'_, TaskStoreRef>,
    tick: State<'_, TickSignal>,
    history: State<'_, HistoryRef>,
) -> Result<(), String> {
    let trigger = find_trigger(&triggers, &id).ok_or_else(|| format!("unknown trigger: {id}"))?;
    if let Some(err) = &trigger.config_error {
        return Err(format!("trigger '{id}' has a configuration error: {err}"));
    }

    let now = now_millis();
    let task = Task {
        id: format!("manual-{id}-{now}"),
        origin: TaskOrigin::Adhoc,
        trigger_id: Some(id.clone()),
        scheduled_at: now,
        created_at: now,
    };
    {
        let mut store = lock_tasks(&task_store);
        store.insert(task.clone());
        save_task_store(&app, &store);
    }
    record_activity(
        &app,
        &history,
        &Activity::new(
            &id,
            ActivityKind::Manual,
            "手動実行を予約しました".to_string(),
        )
        .with_task(&task),
    );
    // 心拍を待たせない。prod の 1 分間隔ではボタンとして成立しないため。
    tick.poke();
    Ok(())
}

#[tauri::command]
fn pause_trigger(id: String, triggers: State<'_, TriggersRef>) -> Result<(), String> {
    match find_trigger(&triggers, &id) {
        Some(t) => {
            t.paused.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("unknown trigger: {id}")),
    }
}

#[tauri::command]
fn resume_trigger(id: String, triggers: State<'_, TriggersRef>) -> Result<(), String> {
    match find_trigger(&triggers, &id) {
        Some(t) => {
            t.paused.store(false, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("unknown trigger: {id}")),
    }
}

/// 登録しようとしているフォルダを下見する (#58)。
///
/// **副作用は無い。** ここで返した宣言を UI が同意画面に出し、確認が取れてから
/// [`register_trigger`] がコピーする。読むだけと入れるので 2 つに分かれているが、検証は
/// 同じ関数を通る (登録側は UI の言うことを信じない)。
fn inspect_candidate(
    triggers: &[TriggerInfo],
    registered_dir: Option<&Path>,
    dir: &Path,
) -> Result<TriggerCandidate, String> {
    let manifest_path = dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(format!(
            "{MANIFEST_FILE} が見つかりません。トリガーのフォルダ ({MANIFEST_FILE} がある階層) を選んでください"
        ));
    }
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{MANIFEST_FILE} を読めません: {e}"))?;
    let manifest: TriggerManifest = serde_json::from_str(&text)
        .map_err(|e| format!("{MANIFEST_FILE} の形式が不正です: {e}"))?;

    // id はコピー先のディレクトリ名になる。焼き込みには無い検証がここだけにあるのは
    // そのため (詳細は [`crate::registry`])。
    validate_trigger_id(&manifest.id)?;

    // 壊れた宣言は登録させない。焼き込みなら「壊れたトリガー」として一覧に残す価値が
    // あるが (タイポしたものが影も形も無くなる方が困る)、まだ入れていないものは
    // 入口で止めた方が直しやすい。
    let validated = validate_manifest(&manifest);
    if let Some(e) = validated.config_error() {
        return Err(e);
    }
    if !dir.join(&manifest.entry).is_file() {
        return Err(format!(
            "entry '{}' がフォルダの中に見つかりません",
            manifest.entry
        ));
    }

    // 衝突判定は in-memory の一覧だけでは足りない。登録直後 (再起動前) のトリガーは
    // ディスクにあって一覧に無いので、両方見る。
    let conflict = match find_trigger(triggers, &manifest.id) {
        Some(t) => Some(t.source.as_str()),
        None => registered_dir
            .filter(|d| d.join(&manifest.id).is_dir())
            .map(|_| TriggerSource::Registered.as_str()),
    };

    Ok(TriggerCandidate {
        path: dir.to_string_lossy().into_owned(),
        id: manifest.id,
        name: manifest.name,
        description: manifest.description,
        schedule: manifest.schedule,
        tz: manifest.tz,
        required_secrets: manifest.required_secrets,
        // 検証済みのパターンから組み立て直す (list_triggers と同じ理由)。同意画面に
        // 出る文字列は、実際に効く宣言と 1 文字も違ってはいけない。
        allowed_hosts: validated.hosts.iter().map(|h| h.as_declared()).collect(),
        conflict,
    })
}

/// フォルダを選ばせて下見する (#58)。キャンセルは `Ok(None)`。
///
/// ダイアログを Rust 側で開くのは、フロントに `@tauri-apps/plugin-dialog` を足さずに
/// 済ませるため。UI が持つのは invoke だけという既存の形を崩さない。
///
/// `command(async)` なのは [`blocking_pick_folder`] を main thread で呼べないため
/// (呼ぶと固まる)。
#[tauri::command(async)]
fn pick_trigger_folder(
    app: AppHandle,
    triggers: State<'_, TriggersRef>,
) -> Result<Option<TriggerCandidate>, String> {
    let Some(picked) = app
        .dialog()
        .file()
        .set_title("トリガーのフォルダを選ぶ")
        .blocking_pick_folder()
    else {
        return Ok(None);
    };
    let dir = picked
        .into_path()
        .map_err(|e| format!("選ばれた場所を解決できません: {e}"))?;
    // 置き場が解決できない環境では衝突判定が in-memory の一覧だけになる。下見を
    // 止めるほどではない (登録側で改めて解決し、そこで失敗する)。
    let registered_dir = registered_triggers_dir(&app).ok();
    inspect_candidate(&triggers, registered_dir.as_deref(), &dir).map(Some)
}

/// トリガーを `<app_data>/triggers/<id>/` に取り込む (#58)。
///
/// **反映は再起動から。** discovery が起動時 1 回で確定することを前提に猶予窓を採って
/// いない (#26) ので、ここで一覧を差し替えるとその前提が崩れる。ホットリロードは V8 の
/// 再初期化と state 継続性を巻き込むので分ける (#55)。
///
/// ファイルの据え方 (staging → 入れ替え) は [`crate::registry::install_trigger`]。
#[tauri::command(async)]
fn register_trigger(
    app: AppHandle,
    path: String,
    triggers: State<'_, TriggersRef>,
    history: State<'_, HistoryRef>,
) -> Result<TriggerCandidate, String> {
    let src = PathBuf::from(&path);
    let dir = registered_triggers_dir(&app)?;
    // UI が渡してきたパスは信じず、下見と同じ検証をやり直す。
    let candidate = inspect_candidate(&triggers, Some(&dir), &src)?;

    if candidate.conflict == Some(TriggerSource::Bundled.as_str()) {
        return Err(format!(
            "'{}' はアプリに同梱されたトリガーと同じ id です。別の id にしてください",
            candidate.id
        ));
    }

    let stats = install_trigger(&src, &dir, &candidate.id)?;

    let replaced = candidate.conflict.is_some();
    record_activity(
        &app,
        &history,
        &Activity::new(
            &candidate.id,
            ActivityKind::Registered,
            format!(
                "{} ({} 個のファイル)。鍵: {} / 宛先: {}",
                if replaced {
                    "トリガーを置き換えました"
                } else {
                    "トリガーを登録しました"
                },
                stats.files,
                describe_declaration(&candidate.required_secrets),
                describe_declaration(&candidate.allowed_hosts),
            ),
        ),
    );
    Ok(candidate)
}

/// 登録されたトリガーを外す (#58)。
///
/// **こちらは再起動を待たない。** 登録と非対称なのは意図的で、「外したのにまだ動く」は
/// 「入れたのにまだ動かない」より実害が大きい。in-memory の一覧からは
/// [`TriggerInfo::unregistered`] で外れ、積まれていた予定はその場で捨てる。
/// JS モジュールはロードされたまま残るが、到達する経路が無くなる。
#[tauri::command]
fn unregister_trigger(
    app: AppHandle,
    id: String,
    triggers: State<'_, TriggersRef>,
    task_store: State<'_, TaskStoreRef>,
    history: State<'_, HistoryRef>,
) -> Result<(), String> {
    validate_trigger_id(&id)?;
    let dir = registered_triggers_dir(&app)?;

    // in-memory に居ないが実体はある = 登録した後まだ再起動していないもの。
    // 「やっぱりやめる」が効かないと、間違って入れたものを再起動するまで外せない。
    let known = find_trigger(&triggers, &id);
    match known {
        Some(t) if t.source == TriggerSource::Bundled => {
            return Err(format!(
                "'{id}' はアプリに同梱されたトリガーなので外せません (停止はできます)"
            ))
        }
        None if !dir.join(&id).is_dir() => return Err(format!("unknown trigger: {id}")),
        _ => {}
    }

    // ファイルを消してから止める。逆にすると、削除に失敗したときに「一覧から消えたのに
    // 再起動で戻ってくる」状態が残る。
    uninstall_trigger(&dir, &id)?;
    if let Some(t) = known {
        t.unregistered.store(true, Ordering::Relaxed);
    }

    let removed = {
        let mut store = lock_tasks(&task_store);
        let removed = store.purge_trigger(&id);
        save_task_store(&app, &store);
        removed
    };
    remove_trigger_state(&app, &id);

    record_activity(
        &app,
        &history,
        &Activity::new(
            &id,
            ActivityKind::Unregistered,
            format!("トリガーを解除しました (未実行の予定 {removed} 件を破棄)"),
        ),
    );
    Ok(())
}

/// アプリを再起動する (#58)。
///
/// 登録の反映に再起動が要る以上、UI から踏める場所に置く。「再起動してください」とだけ
/// 言われて手で落とし直させるのは、常駐アプリでは案外難しい (ウィンドウを閉じても
/// トレイに残る)。
#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart()
}

/// 宣言の一覧を観測面の 1 行に収める。空は「無し」と書く — 空文字だと「記録し忘れ」と
/// 区別がつかない。
fn describe_declaration(values: &[String]) -> String {
    if values.is_empty() {
        "無し".to_string()
    } else {
        values.join(", ")
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

    for t in active_triggers(&triggers) {
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
        // フォルダ選択は Rust 側から開く (#58)。JS からは呼ばせないので capability の
        // 宣言は要らず、エージェント開発者のアプリ側に足す設定も無い。
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_triggers,
            pause_trigger,
            resume_trigger,
            run_trigger_now,
            list_tasks,
            delete_task,
            list_activity,
            list_declared_secrets,
            pick_trigger_folder,
            register_trigger,
            unregister_trigger,
            restart_app,
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

            // dev モードは env-var 単独判定 (compile-time feature 化しない)。緩めるのは
            // 心拍だけ。schedule の下限は DSL パーサが構文として担保しており (`@every` は
            // 5 分以上)、dev の反復手段は手動実行 (`run_trigger_now`) が担う (#26 決定事項 5)。
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
            // 履歴 DB は discovery より先に開く。discovery が出す `[config error]` は
            // 「起動時に emit されて誰にも見えない」代表例なので、必ず永続化する (#42)。
            let history: HistoryRef = Arc::new(Mutex::new(open_history(app.handle())));
            app.manage(history.clone());

            // 2 つ目の走査元: エンドユーザーが実行時に登録したトリガー (#58)。
            // 空でも作っておく — フォルダがそこにあること自体が「直接置く」経路になる。
            let registered_dir = match registered_triggers_dir(app.handle()) {
                Ok(d) => Some(d),
                Err(e) => {
                    eprintln!("registered triggers dir unavailable: {e}");
                    None
                }
            };

            let triggers: TriggersRef = Arc::new(discover_all(
                app.handle(),
                &history,
                &triggers_dir,
                registered_dir.as_deref(),
            ));
            for t in triggers.iter() {
                eprintln!(
                    "discovered trigger: {} ({}) [{}] — entry {}, schedule '{}' tz={:?}",
                    t.manifest.id,
                    t.manifest.name,
                    t.source.as_str(),
                    t.manifest.entry,
                    t.manifest.schedule,
                    t.tz
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
                history,
                secrets_service,
                tick_interval,
            );
            app.manage(TickSignal(Mutex::new(tick_tx)));

            Ok(())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// discovery を通した後の [`TriggerInfo`] 相当。ファイルシステムも Tauri も要らない
    /// 部分 (取捨と可視性) だけをここで固定する。
    fn info(id: &str, source: TriggerSource) -> TriggerInfo {
        TriggerInfo {
            manifest: TriggerManifest {
                id: id.to_string(),
                name: format!("{id} trigger"),
                description: None,
                entry: "index.ts".to_string(),
                required_secrets: Vec::new(),
                allowed_hosts: Vec::new(),
                schedule: "@hourly".to_string(),
                tz: None,
            },
            dir: PathBuf::from(format!("/tmp/{id}")),
            source,
            paused: Arc::new(AtomicBool::new(false)),
            unregistered: Arc::new(AtomicBool::new(false)),
            schedule: Schedule::Hourly { minutes: vec![0] },
            tz: chrono_tz::UTC,
            config_error: None,
            hosts: Vec::new(),
        }
    }

    fn ids(triggers: &[TriggerInfo]) -> Vec<&str> {
        triggers.iter().map(|t| t.manifest.id.as_str()).collect()
    }

    /// 衝突が無ければ両方採用され、id 昇順に並ぶ (実行順序を安定させるため)。
    #[test]
    fn both_sources_are_merged_in_id_order() {
        let (merged, shadowed) = merge_sources(
            vec![info("bravo", TriggerSource::Bundled)],
            vec![
                info("charlie", TriggerSource::Registered),
                info("alpha", TriggerSource::Registered),
            ],
        );

        assert_eq!(ids(&merged), vec!["alpha", "bravo", "charlie"]);
        assert!(shadowed.is_empty());
    }

    /// id が衝突したら焼き込みが勝つ。アプリに同梱された「そのアプリらしさ」を後から
    /// 乗っ取られないため (#58)。
    #[test]
    fn bundled_wins_id_collisions() {
        let (merged, shadowed) = merge_sources(
            vec![info("greeter", TriggerSource::Bundled)],
            vec![info("greeter", TriggerSource::Registered)],
        );

        assert_eq!(ids(&merged), vec!["greeter"]);
        assert_eq!(merged[0].source, TriggerSource::Bundled);
        // 黙って消さない。呼び出し側が観測面に残せるよう返す。
        assert_eq!(ids(&shadowed), vec!["greeter"]);
    }

    // ---- 登録前の下見 (#58) ----------------------------------------------

    /// テスト用の一時ディレクトリ (registry のテストと同じ理由で tempfile は入れない)。
    fn temp_dir(label: &str) -> PathBuf {
        use std::sync::atomic::AtomicU32;
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "chamberlain-inspect-{}-{label}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// トリガーのフォルダを 1 つ作る。`manifest` はそのまま manifest.json に書く。
    fn candidate_dir(root: &Path, manifest: &str) -> PathBuf {
        let dir = root.join("incoming");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), manifest).unwrap();
        std::fs::write(dir.join("index.ts"), "export function tick() {}").unwrap();
        dir
    }

    const VALID_MANIFEST: &str = r#"{
        "id": "probe",
        "name": "下見テスト",
        "entry": "index.ts",
        "schedule": "@daily 09:00",
        "tz": "Asia/Tokyo",
        "requiredSecrets": ["github_token"],
        "allowedHosts": ["API.GitHub.com"]
    }"#;

    /// 同意画面に出す宣言は**検証済みの側**から組み立てる。manifest の生文字列をそのまま
    /// 出すと、大文字の差で「画面に見えている宣言」と「実際に効く宣言」がずれる。
    #[test]
    fn inspect_returns_the_declarations_that_will_be_enforced() {
        let root = temp_dir("valid");
        let dir = candidate_dir(&root, VALID_MANIFEST);

        let c = inspect_candidate(&[], None, &dir).unwrap();

        assert_eq!(c.id, "probe");
        assert_eq!(c.name, "下見テスト");
        assert_eq!(c.schedule, "@daily 09:00");
        assert_eq!(c.required_secrets, vec!["github_token"]);
        assert_eq!(c.allowed_hosts, vec!["api.github.com"]);
        assert_eq!(c.conflict, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 入口で止める条件。壊れたものを入れてから「壊れています」と表示するより、
    /// 入る前に断る方が直しやすい。
    #[test]
    fn inspect_rejects_folders_that_should_not_be_registered() {
        let cases: [(&str, &str); 5] = [
            (
                "bad-id",
                r#"{"id":"../evil","name":"n","entry":"index.ts","schedule":"@hourly"}"#,
            ),
            (
                "reserved-id",
                r#"{"id":"__meta__","name":"n","entry":"index.ts","schedule":"@hourly"}"#,
            ),
            (
                "bad-schedule",
                r#"{"id":"probe","name":"n","entry":"index.ts","schedule":"毎日"}"#,
            ),
            (
                "bad-hosts",
                r#"{"id":"probe","name":"n","entry":"index.ts","schedule":"@hourly","allowedHosts":["*"]}"#,
            ),
            (
                "escaping-entry",
                r#"{"id":"probe","name":"n","entry":"../index.ts","schedule":"@hourly"}"#,
            ),
        ];
        for (label, manifest) in cases {
            let root = temp_dir(label);
            let dir = candidate_dir(&root, manifest);
            assert!(
                inspect_candidate(&[], None, &dir).is_err(),
                "{label} は弾くはず"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// manifest が無いフォルダ / entry が実在しないフォルダも入口で断る。
    #[test]
    fn inspect_requires_a_manifest_and_a_real_entry() {
        let root = temp_dir("missing");
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(inspect_candidate(&[], None, &empty).is_err());

        let dir = candidate_dir(&root, VALID_MANIFEST);
        std::fs::remove_file(dir.join("index.ts")).unwrap();
        let err = inspect_candidate(&[], None, &dir).unwrap_err();
        assert!(err.contains("entry"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 衝突は 2 箇所から拾う: 起動時から居るトリガーと、登録済みだがまだ再起動して
    /// いないもの (ディスクにしか居ない)。
    #[test]
    fn inspect_reports_conflicts_from_memory_and_from_disk() {
        let root = temp_dir("conflict");
        let dir = candidate_dir(&root, VALID_MANIFEST);
        let registered = root.join("registered");
        std::fs::create_dir_all(registered.join("probe")).unwrap();

        let bundled = vec![info("probe", TriggerSource::Bundled)];
        assert_eq!(
            inspect_candidate(&bundled, None, &dir).unwrap().conflict,
            Some("bundled")
        );
        assert_eq!(
            inspect_candidate(&[], Some(&registered), &dir)
                .unwrap()
                .conflict,
            Some("registered")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 解除は再起動を待たない。in-memory の一覧には残るが、そこを見る全経路から外れる。
    #[test]
    fn unregistered_triggers_disappear_from_every_lookup() {
        let triggers = vec![
            info("kept", TriggerSource::Registered),
            info("gone", TriggerSource::Registered),
        ];
        triggers[1].unregistered.store(true, Ordering::Relaxed);

        assert_eq!(
            active_triggers(&triggers)
                .map(|t| t.manifest.id.as_str())
                .collect::<Vec<_>>(),
            vec!["kept"]
        );
        assert!(find_trigger(&triggers, "gone").is_none());
        assert!(find_trigger(&triggers, "kept").is_some());
    }
}
