mod ai;
mod chat;
mod drafts;
mod history;
mod http;
mod permissions;
mod registry;
mod schedule;
mod secrets;
mod tasks;
mod watchdog;
mod worker;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
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

use crate::drafts::DraftDir;
use crate::history::{origin_str, Activity, ActivityKind, HistoryStore, MAX_ROWS, RETENTION};
use crate::permissions::{parse_host_pattern, HostPattern, TriggerGrants, TriggerPermissions};
/// UI に渡る DTO ([`TriggerCandidate`]) に載るので、型も一緒に公開する。
pub use crate::registry::TriggerSource;
use crate::registry::{
    install_trigger, installed_path, is_reserved_id, lint_entry_source, read_entry_source,
    uninstall_trigger, validate_entry_path, validate_registered_entry, validate_trigger_id,
};
use crate::schedule::{parse_schedule, resolve_tz, Schedule};
use crate::secrets::SecretsService;
use crate::tasks::{Task, TaskOrigin, TaskStore};
use crate::watchdog::Watchdog;
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
/// 対処する (同一心拍内のバッチは `now` を共有するので影響しない)。その「させない」の
/// 実体が [`JS_BUDGET`] で、猶予より短く取ってあるので暴走 1 件では破棄まで至らない。
const SCHEDULE_GRACE_TICKS: u32 = 2;

/// JS 実行 1 回に与える予算 (#59)。超えたトリガーは中断され、心拍は次へ進む。
/// 止め方が 2 通り要る理由は [`crate::watchdog`] のモジュール doc にある。
///
/// **値は 2 つの制約に挟まれた狭い窓から選んでいる。**
///
/// - 下限は `chamberlain.ai.complete` の上限 (90s)。これを下回ると、op 自身が許して
///   いる長さの応答待ちを framework が横から殺すことになり、正常系を壊す
/// - 上限は schedule 猶予 (prod で 120s = 心拍 1 分 × [`SCHEDULE_GRACE_TICKS`])。
///   これを上回ると、1 つのトリガーが暴走している間に due になった**他のトリガーの
///   予定が猶予超過で破棄される**。予算が猶予未満なら、他は遅れるだけで実行される
///
/// この上下関係は `the_js_budget_sits_between_its_two_constraints` で固定してある。
/// どちらかの定数を動かすと窓が閉じてそこで気づく。
///
/// 窓が狭いことは、そもそも 1 tick に AI 呼び出しを何度も積むトリガーが構造的に
/// 苦しいことを意味する。上の保証も暴走 1 件まで — 同一心拍で複数が暴走すれば
/// 予算 × 件数まで伸びる。dev (心拍 10 秒 = 猶予 20 秒) でも成り立たないが、
/// dev で予定が流れることは実害として扱わない。
const JS_BUDGET: Duration = Duration::from_secs(110);

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

/// [`TriggerManifest`] が読むキーの全部。**serde は未知のキーを黙って無視する**ので、
/// 配布する仕様書 (#60) の例に綴り違い (`required_secrets` 等) が混ざっていないかを
/// テストで見るために名前を並べておく。構造体にフィールドを足したらここにも足す。
#[cfg(test)]
const MANIFEST_FIELDS: [&str; 8] = [
    "id",
    "name",
    "description",
    "entry",
    "requiredSecrets",
    "allowedHosts",
    "schedule",
    "tz",
];

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
    source: TriggerSource,
}

/// 登録しようとしているトリガーの下見 (#58)。**同意画面に出す内容そのもの。**
///
/// `requiredSecrets` / `allowedHosts` は core が実際に強制している宣言なので (#56 / #57)、
/// ここに出た以上のことはそのトリガーにはできない。宣言が強制力を持つ状態で初めて、
/// 入れる前に見せる意味がある。
///
/// `pub` なのは [`crate::chat::ChatTurn`] に載るため (#61)。秘書が作った下書きも
/// 同じ型で同じ画面に出る — 出どころで見せ方が変わらないことが、この型の役目。
#[derive(Clone, Debug, Serialize)]
pub struct TriggerCandidate {
    /// 選ばれたフォルダの絶対パス。UI はこれをそのまま `register_trigger` に返す。
    pub path: String,
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub schedule: String,
    pub tz: Option<String>,
    #[serde(rename = "requiredSecrets")]
    pub required_secrets: Vec<String>,
    #[serde(rename = "allowedHosts")]
    pub allowed_hosts: Vec<String>,
    /// 同じ id が既にある場合の相手 (`"bundled"` | `"registered"`)。
    ///
    /// `"bundled"` は登録を拒否する — アプリに同梱された「そのアプリらしさ」を後から
    /// 乗っ取られないため。`"registered"` は置き換え (配布物の更新) として通す。
    pub conflict: Option<TriggerSource>,
    /// entry の静的検査で見つかった、仕様から外れている点 (#61)。
    ///
    /// **拒否ではない。**「宣言」(`requiredSecrets` / `allowedHosts`) が「このトリガーに
    /// 何ができるか」を表すのに対し、こちらは「たぶん動かない」を表す。判断材料として
    /// 並べるだけで、登録の可否には効かない ([`crate::registry::lint_entry_source`])。
    pub warnings: Vec<String>,
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

/// Type II (秘書自身の AI) が使ったトークンを、Type I と同じ活動ログに載せる (#71)。
///
/// **`TriggerPermissions` の経路は通れない。** あちらは JS の実行文脈に紐付く記録で、
/// 秘書チャットと下書き生成はそこを通らない。一方これも**エンドユーザーの課金で回る
/// 呼び出し**で、履歴を毎回全部送るチャットこそ一番積み上がる — 消費の観測が Type I に
/// しか無ければ、一番大きい部分が見えないままになる。
///
/// 帰属先はトリガーではないので `__meta__`。kind は Type I と同じ `[ai]` にする —
/// UI から見て意味があるのは「AI に金を使った」という 1 つの概念で、どちら由来かは
/// `source` と本文が持つ (`[denied]` が secret とホストで kind を分けないのと同じ判断)。
///
/// **応答を丸ごと取り、`usage` を呼び出し側に返さない。** 記録を各 call site の作法に
/// すると、`let ai::Response { content, stop, .. }` と書いた 3 つ目の call site の消費が
/// 黙って観測面から消える (`..` は警告を出さない)。中身を取るには記録を通る、という形に
/// しておけば、忘れようがない。切り捨てや失敗の分岐より先に記録されるのも同じ帰結。
///
/// **残すのは応答が返った呼び出しだけ。** Type I が「試行」を残す (キー未設定や API
/// エラーで落ちた回も数える) のは、そこで抑えたいのがトリガー作者による呼び出しの量だから。
/// こちらの目的は課金の可視化で、返ってこなかった呼び出しは課金されていない。
///
/// **prompt も会話の中身も残さない** (#57 の線)。token 数は数値なのでこの線を越えない。
pub(crate) fn record_ai_usage<T>(
    app: &AppHandle,
    history: &HistoryRef,
    what: &str,
    model: Option<&str>,
    response: ai::Response<T>,
) -> (T, ai::StopReason) {
    let message = response
        .usage
        .annotate(format!("{what} model={}", ai::resolve_model(model)));
    record_activity(
        app,
        history,
        &Activity::new(META_NAMESPACE, ActivityKind::AiCall, message),
    );
    (response.content, response.stop)
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

/// 実行時登録の置き場 (`<app_data>/triggers/`)。**起動時に 1 回だけ解決して作る** (#58)。
///
/// 空でも作るのは、「フォルダを開いて直接置く」経路 (#55 の受け取り口の最小形) が
/// そこにあるだけで成立するため。解決に失敗した環境では登録機能だけが使えなくなる
/// (焼き込みトリガーはそのまま動く)。
struct RegisteredDir(Option<PathBuf>);

impl RegisteredDir {
    fn resolve(app: &AppHandle) -> Self {
        Self(match app.path().app_data_dir() {
            Ok(base) => {
                let dir = base.join(REGISTERED_TRIGGERS_DIR);
                match std::fs::create_dir_all(&dir) {
                    Ok(()) => Some(dir),
                    Err(e) => {
                        eprintln!("failed to create {}: {e}", dir.display());
                        None
                    }
                }
            }
            Err(e) => {
                eprintln!("failed to resolve app data dir for registered triggers: {e}");
                None
            }
        })
    }

    /// 登録系コマンドから使う。解決できていない環境では理由を返して断る。
    fn get(&self) -> Result<&Path, String> {
        self.0
            .as_deref()
            .ok_or_else(|| "トリガーの置き場を用意できませんでした".to_string())
    }
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

        // 予約語の一覧は [`crate::registry`] が持つ。焼き込みと実行時登録で違う名前が
        // 通ると、片方の経路からだけ framework の記録に紛れ込める。
        if is_reserved_id(&manifest.id) {
            eprintln!(
                "trigger id '{}' is reserved by framework, skipping {manifest_path:?}",
                manifest.id
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

    let (kept, dropped) = dedupe_by_id(result);
    for t in dropped {
        eprintln!("duplicate trigger id '{}', skipping", t.manifest.id);
    }
    kept
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

    // 焼き込みを先に並べてから同じ dedup にかける。取捨の規則 (先勝ち + id 昇順) は
    // 1 箇所にしかない。
    let (merged, shadowed) = dedupe_by_id(bundled.into_iter().chain(registered).collect());
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

/// id の取捨。戻り値は (採用したもの, 落としたもの)。
///
/// **入力の並び順が優先順位である。** 呼び出し側が先に置いたものが勝つので、2 ソースの
/// merge では焼き込みを先に並べる (#58) — これが「同梱されたトリガーは後から差し替え
/// られない」を担保する唯一の場所になる。
///
/// sort → dedup の順でないと、id 重複時にどちらが生き残るかが read_dir 順 (filesystem 順)
/// 依存で非決定になる (Issue #21 #6)。安定ソートなので、同じ id の中では入力順が保たれる。
fn dedupe_by_id(mut triggers: Vec<TriggerInfo>) -> (Vec<TriggerInfo>, Vec<TriggerInfo>) {
    triggers.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));

    let mut seen = HashSet::new();
    let mut kept = Vec::with_capacity(triggers.len());
    let mut dropped = Vec::new();
    for t in triggers {
        if seen.insert(t.manifest.id.clone()) {
            kept.push(t);
        } else {
            dropped.push(t);
        }
    }
    (kept, dropped)
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
    ///
    /// `Rc` なのは [`Self::run_js`] が `runtime` だけを貸すため。ハンドルを先に手元へ
    /// 取り出す必要があり、`ModuleHandle` の実体はモジュールのソースを抱えている。
    loaded: BTreeMap<String, Rc<rustyscript::ModuleHandle>>,
    history: HistoryRef,
    /// JS 実行 1 回の予算を見張る番犬 (#59)。`runtime` の isolate を外から止めるので、
    /// この構造体と寿命を揃える。
    watchdog: Watchdog,
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
    /// 閉包に貸すのが `&mut Self` ではなく Runtime だけなのはそのため。`Self` を貸すと
    /// 中から `self.runtime.call_function()` が書けてしまい、実行文脈にも番犬 (#59) にも
    /// 掛からない経路が型の上で作れる。ここを通ることは doc の約束ではなく借用の帰結にする。
    ///
    /// 呼び出し元の識別を JS 側に名乗らせると自己申告になるので、Rust 側が実行の前後で
    /// 現在のトリガーを立てる。op はこれを見て manifest の宣言と突き合わせる。
    ///
    /// op は自分では activity を書けない (`AppHandle` を持たない) ので、溜まったものを
    /// ここで回収して観測面に流す (`[denied]` / `[ai]`)。帰属先を持たない記録
    /// (= run_js を通らない経路から来たもの) は、今動かしているトリガーのせいにせず
    /// framework 側 (`__meta__`) に付ける。誤った帰属は観測面としては無いより悪い。
    fn run_js<T>(
        &mut self,
        trigger_id: &str,
        f: impl FnOnce(&mut rustyscript::Runtime) -> Result<T, rustyscript::Error>,
    ) -> Result<T, String> {
        self.with_permissions(|p| p.enter(trigger_id));
        let result = {
            let Self {
                runtime, watchdog, ..
            } = self;
            watchdog.guard(runtime, f)
        };
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
        let Some(handle) = self.loaded.get(trigger_id).cloned() else {
            return Err(format!("trigger '{trigger_id}' is not loaded"));
        };
        self.run_js(trigger_id, |rt| {
            rt.call_function(Some(&handle), "tick", rustyscript::json_args!(ctx))
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

/// トリガーの JS を動かす Runtime の構成。
///
/// **1 箇所にまとめてあるのは、仕様書 (#60) の「できないこと」がここに依存するため。**
/// 拡張を足せば `fetch` や `TextEncoder` の有無が変わり、配布している仕様書が嘘になる。
/// 裏取りのテストが本番と同じ構成を見ていることを、型の上で担保しておく。
fn trigger_runtime_options() -> rustyscript::RuntimeOptions {
    rustyscript::RuntimeOptions {
        extensions: vec![secrets::chamberlain_ops::init()],
        ..Default::default()
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
        // 予算 (#59) 付きで立てる。止め方 2 通りが一組で入ることは guarding 側の責任。
        let (mut runtime, watchdog) = match Watchdog::guarding(JS_BUDGET, trigger_runtime_options())
        {
            Ok(pair) => pair,
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
            watchdog,
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
            // [denied] を踏むことになる。予算 (#59) が掛かるのも同じ理由で、ここを外すと
            // 無限ループを書いたトリガー 1 つで**起動そのもの**が返ってこなくなる。
            let handle = match host.run_js(&t.manifest.id, |rt| rt.load_module(&module)) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("failed to instantiate trigger '{}': {e}", t.manifest.id);
                    host.activity(Activity::new(
                        &t.manifest.id,
                        ActivityKind::InstantiateError,
                        e,
                    ));
                    continue;
                }
            };
            host.loaded.insert(t.manifest.id.clone(), Rc::new(handle));
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
            source: t.source,
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
///
/// 秘書が生成した下書き (#61) もここを通る。**供給元で検証を分けない** — 分ければ、
/// 緩い方を名乗る経路が価値を持つ。
pub(crate) fn inspect_candidate(
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
    let entry_path = dir.join(&manifest.entry);
    if !entry_path.is_file() {
        return Err(format!(
            "entry '{}' がフォルダの中に見つかりません",
            manifest.entry
        ));
    }
    // 「今この場所にある」だけでは足りない。コピーを生き延びない置き方 (ドット始まりの
    // 下) は、入れた後に load error として現れるので入口で断る。
    validate_registered_entry(&manifest.entry)?;

    // 中身も見る (#61)。宣言と違って強制力は無いが、**書いたのが AI なら誰も読んで
    // いない**ので、機械が読んで同意画面に並べる。
    let source = read_entry_source(&entry_path, &manifest.entry)?;
    let warnings = lint_entry_source(&source)?;

    // 衝突判定は in-memory の一覧だけでは足りない。登録直後 (再起動前) のトリガーは
    // ディスクにあって一覧に無いので、両方見る。
    let conflict = match find_trigger(triggers, &manifest.id) {
        Some(t) => Some(t.source),
        None => registered_dir
            .filter(|d| installed_path(d, &manifest.id).is_dir())
            .map(|_| TriggerSource::Registered),
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
        warnings,
    })
}

/// フォルダ選択ダイアログを開く。キャンセルは `Ok(None)`。
///
/// ダイアログを Rust 側で開くのは、フロントに `@tauri-apps/plugin-dialog` を足さずに
/// 済ませるため (#58)。UI が持つのは invoke だけという既存の形を崩さない。
///
/// **blocking pool の中から呼ぶこと。** ダイアログが開いている間そのスレッドは返らず、
/// ユーザーが選び終わるまでの時間に上限は無い。`blocking_pick_folder` 自体、main thread
/// からは呼べない。
fn pick_folder(app: &AppHandle, title: &str) -> Result<Option<PathBuf>, String> {
    let Some(picked) = app.dialog().file().set_title(title).blocking_pick_folder() else {
        return Ok(None);
    };
    picked
        .into_path()
        .map(Some)
        .map_err(|e| format!("選ばれた場所を解決できません: {e}"))
}

/// フォルダを選ばせてトリガーを下見する (#58)。キャンセルは `Ok(None)`。
#[tauri::command]
async fn pick_trigger_folder(
    app: AppHandle,
    triggers: State<'_, TriggersRef>,
    registered: State<'_, RegisteredDir>,
) -> Result<Option<TriggerCandidate>, String> {
    let triggers = Arc::clone(&triggers);
    // 置き場が解決できない環境では衝突判定が in-memory の一覧だけになる。下見を
    // 止めるほどではない (登録側で改めて解決し、そこで断る)。
    let registered_dir = registered.0.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let Some(dir) = pick_folder(&app, "トリガーのフォルダを選ぶ")? else {
            return Ok(None);
        };
        inspect_candidate(&triggers, registered_dir.as_deref(), &dir).map(Some)
    })
    .await
    .map_err(|e| format!("フォルダの選択に失敗しました: {e}"))?
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
    registered: State<'_, RegisteredDir>,
    drafts: State<'_, DraftDir>,
    history: State<'_, HistoryRef>,
) -> Result<TriggerCandidate, String> {
    let src = PathBuf::from(&path);
    // 秘書が書いたものか、人が選んだフォルダか (#61)。**判断には使わない** — 検証も
    // コピーも同じで、変わるのは観測面に残る 1 行だけ。
    let from_draft = drafts.contains(&src);
    let dir = registered.get()?;
    // 下見の結果は UI が持って戻ってくるだけなので、manifest を読み直して同じ検証を
    // やり直す (フォルダは下見の後に書き換わりうる)。**選ばれたフォルダそのものは
    // 信じる** — 同意とパスを機構で結ぶには、core 側で発行したトークンを介す必要がある。
    let candidate = inspect_candidate(&triggers, Some(dir), &src)?;

    if candidate.conflict == Some(TriggerSource::Bundled) {
        return Err(format!(
            "'{}' はアプリに同梱されたトリガーと同じ id です。別の id にしてください",
            candidate.id
        ));
    }

    // 置き換え先は id で決まるが、既に居る実体のディレクトリ名は id と違いうる
    // (`<app_data>/triggers/` に手で置く経路)。畳まずに据えると同じ id の実体が 2 つ残り、
    // 次の起動でどちらが勝つかが read_dir 順に落ちる (#21 / #6 で消したはずの非決定性)。
    // 「置き換えました」と言う以上、古い方はここで消す。
    let superseded = find_trigger(&triggers, &candidate.id)
        .filter(|t| t.source == TriggerSource::Registered)
        .map(|t| t.dir.clone())
        .filter(|prev| prev != &installed_path(dir, &candidate.id));

    let stats = install_trigger(&src, dir, &candidate.id)?;

    if let Some(prev) = superseded {
        // 据え置きは終わっているので、ここで失敗しても新しい方は入っている。
        // 黙って残すと次の起動で衝突するため、観測面には残す。
        if let Err(e) = uninstall_trigger(&prev) {
            eprintln!(
                "failed to remove the superseded registration of '{}' at {}: {e}",
                candidate.id,
                prev.display()
            );
        }
    }

    // 下書きは据え置きが終わった時点で用済み。残すと、次の起動まで「同意した覚えの
    // ないトリガーの元」がディスクに残る。
    if from_draft {
        if let Err(e) = drafts::discard(drafts.get()?, &candidate.id) {
            eprintln!("failed to discard the draft of '{}': {e}", candidate.id);
        }
    }

    let replaced = candidate.conflict.is_some();
    // 読み飛ばした数も書く。`.git` ごと選んだときに「入っていないもの」が分かる。
    let skipped = if stats.skipped > 0 {
        format!(" / 読み飛ばし {} 件", stats.skipped)
    } else {
        String::new()
    };
    record_activity(
        &app,
        &history,
        &Activity::new(
            &candidate.id,
            ActivityKind::Registered,
            format!(
                "{}{} ({} 個のファイル{skipped})。鍵: {} / 宛先: {}",
                if from_draft { "秘書が作った" } else { "" },
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

/// 秘書が作った下書きを捨てる (#61)。同意画面で「やめる」を押したときの後始末。
///
/// **見送ったこと自体は記録しない。** 下書きを作ったことは `[drafted]` に残っており、
/// そこに `[registered]` が続かなければ「入れなかった」と読める。断るたびに行が増えると、
/// 観測面が「ユーザーが何を断ったか」の記録になってしまう。
#[tauri::command]
fn discard_trigger_draft(id: String, drafts: State<'_, DraftDir>) -> Result<(), String> {
    drafts::discard(drafts.get()?, &id)
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
    registered: State<'_, RegisteredDir>,
    task_store: State<'_, TaskStoreRef>,
    history: State<'_, HistoryRef>,
) -> Result<(), String> {
    validate_trigger_id(&id)?;
    let dir = registered.get()?;

    let known = find_trigger(&triggers, &id);
    if let Some(t) = known {
        if t.source == TriggerSource::Bundled {
            return Err(format!(
                "'{id}' はアプリに同梱されたトリガーなので外せません (停止はできます)"
            ));
        }
    }

    // 消す先は discovery が見つけた実体そのもの。id から組み立て直すと、手で置いた
    // フォルダ (名前が id と違いうる) を「外したつもりで何も消していない」が起きる。
    // in-memory に居ないものは登録直後 (再起動前) なので、そちらは規定の位置にある。
    let target = known
        .map(|t| t.dir.clone())
        .unwrap_or_else(|| installed_path(dir, &id));

    // ファイルを消してから止める。逆にすると、削除に失敗したときに「一覧から消えたのに
    // 再起動で戻ってくる」状態が残る。
    let existed = uninstall_trigger(&target)?;
    if known.is_none() && !existed {
        return Err(format!("unknown trigger: {id}"));
    }
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

/// 仕様書を skill として書き出すときのフォルダ名 (#60)。
///
/// Claude Code / Claude Desktop は `<skills root>/<name>/SKILL.md` を探すので、この形で
/// 書き出せば、選んだ先が skills ディレクトリでありさえすればそのまま載る。**仕様書の
/// frontmatter の `name` と一致していなければならない** (テストで固定してある)。
const TRIGGER_SKILL_DIR: &str = "chamberlain-triggers";

/// トリガーの書き方を 1 ファイルで説明した仕様書 (#60)。**core に焼き込む。**
///
/// 実行時登録 (#58) でトリガーの供給元が 3 つ (エンドユーザー自身 / 配布元 / 秘書) に
/// 開いた以上、「仕様を知らない書き手」が最も多い経路は**エンドユーザーが外部の生成 AI に
/// 書かせる**ところになる。渡すものが `docs/architecture.md` しか無ければ、framework 本体の
/// 実装の話に埋もれた仕様を食わせることになり、まともなトリガーは出てこない。
///
/// バイナリに焼くのは、**仕様書と実装のバージョンを機械的に一致させる**ため。resource dir に
/// 置くとエージェント開発者の `bundle.resources` の書き方に依存し、core を上げても手元の
/// 仕様書が古いまま、という乖離が起きる。同じ実体が `create-chamberlain` の
/// scaffold 出力にも skill として配られる (同期は `scripts/sync-template.mjs`)。
const TRIGGER_SPEC: &str = include_str!("trigger-spec.md");

/// 仕様書を skill として書き出す (#60)。キャンセルは `Ok(None)`、成功は書いた場所。
///
/// **配るのは「本文をコピーさせる」形ではなく skill の形にした。** 貼り付けさせると、
/// AI が返した 2 ファイルをエンドユーザーが手で作って保存することになり、TS を書けない
/// 人向けの経路としてそこだけ人力で残る。skill として載れば AI がフォルダごと書き出せる
/// ので、ユーザーの操作は「[フォルダから追加…] でそれを指す」だけになる。
///
/// チャット窓しか持たない相手向けの口はアプリに持たない。そちらは将来、AI が
/// Chamberlain の公開ドキュメントを自分で参照する形に寄せる。
///
/// ダイアログを待つので [`pick_folder`] と同じく blocking pool の中で動かす。
#[tauri::command]
async fn save_trigger_skill(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(root) = pick_folder(
            &app,
            "skill の置き場所を選ぶ (Claude Code なら .claude/skills)",
        )?
        else {
            return Ok(None);
        };
        let dir = root.join(TRIGGER_SKILL_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("{} を作れません: {e}", dir.display()))?;
        let file = dir.join("SKILL.md");
        std::fs::write(&file, TRIGGER_SPEC)
            .map_err(|e| format!("{} に書けません: {e}", file.display()))?;
        Ok(Some(file.display().to_string()))
    })
    .await
    .map_err(|e| format!("skill の保存に失敗しました: {e}"))?
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
            save_trigger_skill,
            pick_trigger_folder,
            register_trigger,
            unregister_trigger,
            discard_trigger_draft,
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
            // 登録系コマンドも同じ値を使い回す (解決と mkdir を毎回やり直さない)。
            let registered_dir = RegisteredDir::resolve(app.handle());
            // 秘書が書いた下書きの置き場 (#61)。走査先ではないので discovery には渡さない
            // — 同意を取る前の生成物が起動で勝手に動き出さないための線引き。
            app.manage(DraftDir::resolve(app.handle()));

            let triggers: TriggersRef = Arc::new(discover_all(
                app.handle(),
                &history,
                &triggers_dir,
                registered_dir.0.as_deref(),
            ));
            app.manage(registered_dir);
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

    /// JS の予算は 2 つの定数に挟まれている (#59)。どちらかを動かして窓が閉じたら
    /// ここで気づく — 気づかないと「正常な AI 応答を殺す」か「暴走 1 件が他トリガーの
    /// 予定を落とす」のどちらかが静かに戻る。根拠は [`JS_BUDGET`] の doc。
    #[test]
    fn the_js_budget_sits_between_its_two_constraints() {
        assert!(
            JS_BUDGET > Duration::from_secs(ai::ANTHROPIC_TIMEOUT_SECS),
            "予算が ai.complete の上限以下: op 自身が許す長さの応答待ちを横から殺す"
        );
        assert!(
            JS_BUDGET < TICK_INTERVAL_PROD * SCHEDULE_GRACE_TICKS,
            "予算が schedule 猶予以上: 暴走 1 件で他トリガーの予定が破棄される"
        );
    }

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
        let (merged, shadowed) = dedupe_by_id(vec![
            info("bravo", TriggerSource::Bundled),
            info("charlie", TriggerSource::Registered),
            info("alpha", TriggerSource::Registered),
        ]);

        assert_eq!(ids(&merged), vec!["alpha", "bravo", "charlie"]);
        assert!(shadowed.is_empty());
    }

    /// id が衝突したら**先に並べた方**が勝つ。discover_all が焼き込みを先に置くので、
    /// アプリに同梱された「そのアプリらしさ」は後から乗っ取られない (#58)。
    #[test]
    fn the_first_source_wins_id_collisions() {
        let (merged, shadowed) = dedupe_by_id(vec![
            info("greeter", TriggerSource::Bundled),
            info("greeter", TriggerSource::Registered),
        ]);

        assert_eq!(ids(&merged), vec!["greeter"]);
        assert_eq!(merged[0].source, TriggerSource::Bundled);
        // 黙って消さない。呼び出し側が観測面に残せるよう返す。
        assert_eq!(ids(&shadowed), vec!["greeter"]);
        assert_eq!(shadowed[0].source, TriggerSource::Registered);
    }

    // ---- 登録前の下見 (#58) ----------------------------------------------

    use crate::registry::temp_dir;

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

    /// entry の中身も下見の対象 (#61)。**動かないことが確定しているものは入口で断る。**
    /// 供給元 ((a) 人が選んだフォルダ / (c) 秘書の生成) で分岐しない。
    #[test]
    fn inspect_rejects_an_entry_without_a_tick_export() {
        let root = temp_dir("no-tick");
        let dir = candidate_dir(&root, VALID_MANIFEST);
        std::fs::write(dir.join("index.ts"), "export function run() {}").unwrap();

        let err = inspect_candidate(&[], None, &dir).unwrap_err();

        assert!(err.contains("tick"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 仕様から外れているだけのものは同意画面に並べる。**登録は止めない** — 文字列
    /// マッチで拒否すると、動くトリガーを断りうる。
    #[test]
    fn inspect_reports_spec_violations_as_warnings() {
        let root = temp_dir("warn");
        let dir = candidate_dir(&root, VALID_MANIFEST);
        std::fs::write(
            dir.join("index.ts"),
            "import { x } from \"./h.ts\";\nexport function tick() {}",
        )
        .unwrap();

        let c = inspect_candidate(&[], None, &dir).unwrap();

        assert_eq!(c.warnings.len(), 1, "{:?}", c.warnings);
        assert!(c.warnings[0].contains("import"));
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
            Some(TriggerSource::Bundled)
        );
        assert_eq!(
            inspect_candidate(&[], Some(&registered), &dir)
                .unwrap()
                .conflict,
            Some(TriggerSource::Registered)
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

    /// 仕様書 ([`TRIGGER_SPEC`]) が実装と食い違っていないことを固定する (#60)。
    ///
    /// **これは配布物である。** 中身がずれると、エンドユーザーが外部の生成 AI に渡した
    /// 指示書がそのまま「動かないトリガーを書かせる指示書」になり、しかも間違いに
    /// 気づくのは登録して再起動した後になる。人が読み比べて保つ約束にはできない。
    mod trigger_spec_doc {
        use super::*;

        /// 仕様書中の ```json ブロック。
        ///
        /// 閉じフェンスが見つからないブロックは飛ばさず panic する。飛ばすと「例を 1 つ
        /// 足して、それが検査されていなかった」が下の件数チェックをすり抜ける。
        fn json_blocks() -> Vec<&'static str> {
            TRIGGER_SPEC
                .split("```json\n")
                .skip(1)
                .map(|rest| {
                    rest.split_once("\n```")
                        .unwrap_or_else(|| panic!("閉じていない json ブロックがある:\n{rest}"))
                        .0
                })
                .collect()
        }

        /// `<!-- spec-test: NAME -->` の後に来る表から、**1 列目**のインラインコードを拾う。
        ///
        /// 1 列目に閉じるので、「代わりにこう書く」の列に正しい記法が入っていても拾わない
        /// (行全体から最初のコードを探すと、1 列目が地の文の行でその列を掴んでしまう)。
        /// マーカーで括るのは、仕様書に表を足したときにこのテストが黙って素通りしない
        /// ようにするため。
        fn marked_table_codes(marker: &str) -> Vec<&'static str> {
            let anchor = format!("<!-- spec-test: {marker} -->");
            let rest = TRIGGER_SPEC
                .split_once(&anchor)
                .unwrap_or_else(|| panic!("仕様書にマーカー '{anchor}' がない"))
                .1;
            rest.lines()
                .map(str::trim)
                .skip_while(|line| !line.starts_with('|'))
                .take_while(|line| line.starts_with('|'))
                // 行頭の `|` で split すると先頭が空文字になるので、1 列目は nth(1)。
                .filter_map(|line| line.split('|').nth(1))
                .filter_map(|cell| {
                    let (_, after) = cell.split_once('`')?;
                    after.split_once('`').map(|(code, _)| code)
                })
                .collect()
        }

        /// 仕様書中の ```typescript ブロックのうち、トリガー 1 個として完結しているもの。
        /// 型の抜粋 (`declare const chamberlain` 等) は export を持たないので落ちる。
        fn trigger_typescript_blocks() -> Vec<&'static str> {
            TRIGGER_SPEC
                .split("```typescript\n")
                .skip(1)
                .map(|rest| {
                    rest.split_once("\n```")
                        .unwrap_or_else(|| {
                            panic!("閉じていない typescript ブロックがある:\n{rest}")
                        })
                        .0
                })
                .filter(|block| block.contains("export") && block.contains("tick"))
                .collect()
        }

        /// 仕様書どおりに書いた index.ts が、同意画面の静的検査を無傷で通ること (#61)。
        ///
        /// **警告 0 件まで見る。** ここがずれると、仕様どおりのトリガーに嘘の警告が出る。
        /// `chamberlain.http.fetch` を素の `fetch` と取り違える類の誤検知は、配布物の
        /// 例が一番早く見つける。
        #[test]
        fn index_samples_pass_the_entry_lint() {
            let blocks = trigger_typescript_blocks();
            assert!(
                blocks.len() >= 4,
                "index.ts の例が {} 件しかない",
                blocks.len()
            );
            for block in blocks {
                match lint_entry_source(block) {
                    Ok(warnings) => assert!(
                        warnings.is_empty(),
                        "仕様書の例に警告が出る: {warnings:?}\n{block}"
                    ),
                    Err(e) => panic!("仕様書の例が入口で断られる: {e}\n{block}"),
                }
            }
        }

        /// skill として書き出す以上、frontmatter が要る (#60)。**name は書き出し先の
        /// フォルダ名と一致していなければならない** — ずれると Claude 側が skill として
        /// 認識しない。`description` は「いつ読むか」の判断材料なので空を許さない。
        #[test]
        fn skill_frontmatter_matches_the_output_path() {
            let body = TRIGGER_SPEC
                .strip_prefix("---\n")
                .expect("仕様書は frontmatter で始まる");
            let (front, _) = body
                .split_once("\n---\n")
                .expect("frontmatter が閉じていない");
            let field = |key: &str| {
                front
                    .lines()
                    .find_map(|l| l.strip_prefix(&format!("{key}: ")))
                    .unwrap_or_else(|| panic!("frontmatter に {key} がない"))
                    .trim()
            };
            assert_eq!(field("name"), TRIGGER_SKILL_DIR);
            assert!(field("description").len() > 30, "description が短すぎる");
        }

        /// 仕様書に載せた manifest がそのまま discovery を通ること。
        ///
        /// **断片を許さない** — 読めないブロックは黙って飛ばさず落とす。飛ばすようにすると
        /// 「例を 1 つ足して、それが壊れていた」がテストの外に出る。それはこのテストが
        /// 守ろうとしているものそのものである。
        #[test]
        fn manifest_samples_pass_validation() {
            let blocks = json_blocks();
            // 例を減らしたときに「0 件検査して成功」にならないよう下限を置く。
            assert!(
                blocks.len() >= 4,
                "manifest の例が {} 件しかない",
                blocks.len()
            );
            for block in blocks {
                let manifest = serde_json::from_str::<TriggerManifest>(block).unwrap_or_else(|e| {
                    panic!("仕様書の json が manifest として読めない: {e}\n{block}")
                });
                // 綴り違いは serde が黙って無視するので、キーの側も見る。`required_secrets`
                // と書いた例を配ると「宣言したのに読めないトリガー」を教えることになる。
                let raw: BTreeMap<String, serde_json::Value> =
                    serde_json::from_str(block).expect("json object");
                for key in raw.keys() {
                    assert!(
                        MANIFEST_FIELDS.contains(&key.as_str()),
                        "仕様書の manifest に未知のキー '{key}' がある (綴り違い?)"
                    );
                }
                let validated = validate_manifest(&manifest);
                assert!(
                    validated.config_error().is_none(),
                    "仕様書の manifest '{}' が構成エラー: {:?}",
                    manifest.id,
                    validated.config_error()
                );
                // 仕様書は「フォルダ名 = id」「entry は index.ts」と指示している。
                assert_eq!(manifest.entry, "index.ts");
                assert!(
                    registry::validate_trigger_id(&manifest.id).is_ok(),
                    "仕様書の id '{}' は実行時登録の検証を通らない",
                    manifest.id
                );
            }
        }

        /// §5.2 が書いている `ai.complete` の上限が実装と一致していること (#68)。
        ///
        /// 切り捨てを例外にした以上、**この数字は逃げ道の説明**になっている。ずれると
        /// 「仕様書どおりに `maxTokens` を指定したのに範囲外で断られる」が起きる。
        #[test]
        fn documented_ai_limits_match_the_implementation() {
            for value in [ai::DEFAULT_MAX_TOKENS, ai::MAX_ALLOWED_MAX_TOKENS] {
                assert!(
                    TRIGGER_SPEC.contains(&value.to_string()),
                    "仕様書 §5.2 に {value} が出てこない"
                );
            }
            assert!(
                TRIGGER_SPEC.contains("maxTokens"),
                "仕様書が maxTokens を説明していない"
            );
        }

        /// 「使える記法」の表が全部パースを通ること。
        #[test]
        fn documented_schedules_parse() {
            let codes = marked_table_codes("schedule-ok");
            assert!(codes.len() >= 10, "表が短すぎる: {codes:?}");
            for code in codes {
                assert!(
                    parse_schedule(code).is_ok(),
                    "仕様書が使えると書いている '{code}' がパースを通らない"
                );
            }
        }

        /// 「使えない記法」の表が全部弾かれること。**こちらが本体**である。
        /// 生成 AI は放っておくと cron 式や `@every 7m` を書くので、
        /// 採らない記法を名指しで挙げているのが仕様書の効きどころになる。
        #[test]
        fn rejected_schedules_are_really_rejected() {
            let codes = marked_table_codes("schedule-reject");
            assert!(codes.len() >= 6, "表が短すぎる: {codes:?}");
            for code in codes {
                assert!(
                    parse_schedule(code).is_err(),
                    "仕様書が使えないと書いている '{code}' がパースを通ってしまう"
                );
            }
        }

        /// 仕様書の「できないこと」(§6) が実際の JS 環境と一致していること。
        ///
        /// **ここは推測で書けない。** rustyscript が引く deno 拡張が増減すれば
        /// `fetch` や `TextEncoder` の有無は変わる。仕様書が「存在しません」と断言する
        /// 以上、断言の裏取りは実物の isolate に対して行う。
        ///
        /// V8 isolate を 1 つ立てるので、**このモジュールで Runtime を作るテストは
        /// これ 1 つに保つこと** (同一スレッドで 2 つ目を作ると V8 が abort する)。
        #[test]
        fn spec_matches_the_real_js_environment() {
            // 本番の worker (spawn_trigger_worker) と同じ構成で立てる。ここが分かれると
            // 「実物に対する裏取り」という前提が崩れ、別の isolate に対して合格し続ける。
            let mut runtime =
                rustyscript::Runtime::new(trigger_runtime_options()).expect("JS runtime");

            let absent = [
                "fetch",
                "TextEncoder",
                "TextDecoder",
                "structuredClone",
                "AbortController",
                "process",
                "require",
                "localStorage",
                "window",
                "document",
            ];
            let present = [
                "console",
                "crypto",
                "URL",
                "URLSearchParams",
                "atob",
                "btoa",
                "setTimeout",
                "setInterval",
                "Intl",
                "chamberlain",
                // 使えると勧めているわけではないが、**見えている**ので仕様書が黙って
                // いると「ここに書いていない機能は存在しません」が嘘になる (§6)。
                "Deno",
            ];
            // eval 1 回にまとめる。1 名前ごとに呼ぶと、その都度スクリプトのコンパイルと
            // イベントループの完走が走る (isolate を 1 つに抑えた意味が薄れる)。
            let names: Vec<&str> = absent.iter().chain(present.iter()).copied().collect();
            let script = format!(
                "{}.map((n) => typeof globalThis[n])",
                serde_json::to_string(&names).expect("json")
            );
            let kinds: Vec<String> = runtime.eval(script).expect("typeof を引けない");
            for (name, kind) in names.iter().zip(&kinds) {
                if absent.contains(name) {
                    assert_eq!(kind, "undefined", "仕様書は {name} を無いと書いている");
                } else {
                    assert_ne!(kind, "undefined", "仕様書は {name} を使えると書いている");
                }
                // 名前は仕様書 §6 から拾えないので (表の書式に乗らない)、両者が同じものを
                // 指していることだけは固定する。片方から消えたらここで気づく。
                assert!(
                    TRIGGER_SPEC.contains(name),
                    "仕様書に出てこない名前を検査している: {name}"
                );
            }

            // 相対 import が通らないこと (仕様書は「1 ファイルに全部書く」と指示している)。
            let dir = temp_dir("spec-import");
            std::fs::write(
                dir.join("helper.ts"),
                "export const hello = () => \"hi\";\n",
            )
            .expect("helper");
            std::fs::write(
                dir.join("index.ts"),
                "import { hello } from \"./helper.ts\";\nexport function tick() { return { notify: { body: hello() } }; }\n",
            )
            .expect("entry");
            let module = rustyscript::Module::load(dir.join("index.ts")).expect("load");
            assert!(
                runtime.load_module(&module).is_err(),
                "相対 import が通るなら仕様書 §6 を書き換えること"
            );
        }
    }
}
