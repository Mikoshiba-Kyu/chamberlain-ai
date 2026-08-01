//! 実行履歴レイヤ (#42 / #26 Phase 2)。
//!
//! Phase 1 で「未来への意図」がタスクリストとして実体を持った。その対になる**過去の記録**が
//! ここである。タスクリストは pending のみを持ち完了したものは即座に消える (決定事項 1) ので、
//! 「何が起きたか」を残す場所が別に要る。
//!
//! # なぜ SQLite か
//!
//! 履歴は追記専用 + 時系列クエリ + retention という形で、`tauri-plugin-store` の JSON とは
//! アクセスパターンが違う。JSON では追記のたびに `save()` がファイル全体を書き直し、
//! 「直近 N 件」もメモリに全部載せてからのフィルタになる。
//!
//! タスクリスト (`tasks.json`) は移していない。あちらは小さい可変の作業セットで、
//! **エンドユーザーが手で編集できること**を設計に織り込んでいる ([`crate::tasks::TaskStore::normalize`]
//! の存在理由)。性質が違うので保存先も分ける。
//!
//! # 起動時イベントの gap がこれで閉じる
//!
//! `emit_activity` は Tauri の `Emitter::emit` で JS に飛ばすだけだったので、worker が
//! `.setup()` 内で起動する都合上、**webview のリスナーが繋がる前のイベントは捨てられていた**
//! (`[schedule error]` / `[expanded]` / `[rescheduled]` / `[orphaned]`)。永続化すれば UI は
//! 起動後に保存済みの履歴を読めばよく、接続タイミングに依存しなくなる。
//!
//! # kind を列に出す
//!
//! 0.2.0 まで種別は `message` 文字列のプレフィックス (`[skipped]` 等) で表現されていた。
//! フィルタや集計のたびに文字列パースが要るので、[`ActivityKind`] を独立した列にする。
//! **DB 上は enum ではなく TEXT** である。将来使われなくなった kind の行も読めなければ
//! ならないため (retention 30 日でも移行期間中は古い行が残る)。
//!
//! 表示用のプレフィックスは [`ActivityKind::prefix`] から組み立てる。文字列側とマッピングを
//! 二重に持たないための措置で、UI から見た `message` は 0.2.0 と同じ形のまま。

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection};

use crate::tasks::{Task, TaskOrigin};

/// 履歴の保持期間。これより古い行は掃除される。
pub(crate) const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// 保持する行数の上限。日数だけだと `@every 5m` (288 件/日) を入れた環境で
/// 1 ヶ月 8,640 行を超えて膨らむので併用する。
pub(crate) const MAX_ROWS: usize = 20_000;

/// 掃除を起こす間隔。心拍ごとに時刻を比べるだけなのでコストは無視できる
/// (時刻イベントにすると起動保証の問題が出るのは #26 決定事項 6 と同じ理由)。
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 履歴イベントの種別。
///
/// 0.2.0 の activity プレフィックス一覧をそのまま列にしたもの。値は DB に入る安定した
/// 識別子なので、**一度出荷した文字列は変えない** (古い行が読めなくなる)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    /// トリガーが返した通知本文。唯一エンドユーザー向けのイベント。
    Notify,
    /// tick() の実行が失敗した。
    Error,
    /// トリガーの JS がロードできなかった。
    LoadError,
    /// トリガーの JS がインスタンス化できなかった。
    InstantiateError,
    /// discovery 時点の schedule / tz の構成エラー。
    ScheduleError,
    /// 展開器がタスクを積んだ。
    Expanded,
    /// schedule 変更を検知して再展開した。
    Rescheduled,
    /// 実行対象トリガーが manifest から消えたため破棄した。
    Orphaned,
    /// トリガーが実行できない状態 (構成エラー / ロード失敗) のため破棄した。
    Unavailable,
    /// 停止中のため破棄した。
    Paused,
    /// schedule 由来の予定が猶予を超えて遅れたため破棄した。
    Skipped,
    /// ad-hoc の予定が猶予 (24h) を超えたため破棄した。
    Expired,
    /// トリガーを持たないタスクは Phase 1 では実行経路が無い。
    Unsupported,
    /// 手動実行を予約した。
    Manual,
    /// 予定が削除された。
    Deleted,
}

impl ActivityKind {
    /// DB に入る識別子。**変えないこと。**
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Error => "error",
            Self::LoadError => "load_error",
            Self::InstantiateError => "instantiate_error",
            Self::ScheduleError => "schedule_error",
            Self::Expanded => "expanded",
            Self::Rescheduled => "rescheduled",
            Self::Orphaned => "orphaned",
            Self::Unavailable => "unavailable",
            Self::Paused => "paused",
            Self::Skipped => "skipped",
            Self::Expired => "expired",
            Self::Unsupported => "unsupported",
            Self::Manual => "manual",
            Self::Deleted => "deleted",
        }
    }

    /// UI に出る表示用プレフィックス。通知本文だけは前置きしない
    /// (エンドユーザーが読むのは秘書の言葉であって framework の分類ではない)。
    pub(crate) fn prefix(&self) -> Option<&'static str> {
        match self {
            Self::Notify => None,
            Self::Error => Some("[error]"),
            Self::LoadError => Some("[load error]"),
            Self::InstantiateError => Some("[instantiate error]"),
            Self::ScheduleError => Some("[schedule error]"),
            Self::Expanded => Some("[expanded]"),
            Self::Rescheduled => Some("[rescheduled]"),
            Self::Orphaned => Some("[orphaned]"),
            Self::Unavailable => Some("[unavailable]"),
            Self::Paused => Some("[paused]"),
            Self::Skipped => Some("[skipped]"),
            Self::Expired => Some("[expired]"),
            Self::Unsupported => Some("[unsupported]"),
            Self::Manual => Some("[manual]"),
            Self::Deleted => Some("[deleted]"),
        }
    }
}

/// イベントの元になったタスクのスナップショット。
///
/// タスクは実行後に消えるので**参照整合性は取れない**。外部キーではなくスナップショットとして
/// 持つ。これがあると「積まれた予定 → その結末」を後から突き合わせられる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskRef {
    pub id: String,
    pub origin: TaskOrigin,
    /// 実行を意図された時刻。実際の時刻は [`ActivityRecord::ts`] にあり、遅延は
    /// 2 つの差として導出する (追加決定 11 と同じ考え方でフィールドに持たない)。
    pub scheduled_at: u64,
}

impl TaskRef {
    pub(crate) fn of(task: &Task) -> Self {
        Self {
            id: task.id.clone(),
            origin: task.origin,
            scheduled_at: task.scheduled_at,
        }
    }
}

/// 観測面に流す 1 件。live emit (Tauri event) と永続化の両方がこれを受け取る。
#[derive(Debug, Clone)]
pub(crate) struct Activity {
    /// トリガー ID。トリガーに紐付かないものは `"__task__"`。
    pub source: String,
    pub kind: ActivityKind,
    /// プレフィックスを含まない本文。表示用の文字列は [`Activity::display`] が組み立てる。
    pub message: String,
    pub task: Option<TaskRef>,
}

impl Activity {
    pub(crate) fn new(source: impl Into<String>, kind: ActivityKind, message: String) -> Self {
        Self {
            source: source.into(),
            kind,
            message,
            task: None,
        }
    }

    /// 元になったタスクを紐付ける。
    pub(crate) fn with_task(mut self, task: &Task) -> Self {
        self.task = Some(TaskRef::of(task));
        self
    }

    /// UI に出す 1 行。0.2.0 の `message` と同じ形 (`[skipped] ...`) を kind から組み立てる。
    pub(crate) fn display(&self) -> String {
        match self.kind.prefix() {
            Some(prefix) => format!("{prefix} {}", self.message),
            None => self.message.clone(),
        }
    }
}

/// 永続化された履歴 1 行。読み出し側 (UI / テスト) が受け取る形。
#[derive(Debug, Clone)]
pub(crate) struct ActivityRecord {
    /// 行 id。UI が live emit と突き合わせるための同一性。
    pub id: i64,
    pub ts: u64,
    pub source: String,
    /// **`ActivityKind` ではなく `String`**。使われなくなった kind の行も読めなければ
    /// ならないため (モジュール doc 参照)。
    pub kind: String,
    /// プレフィックスを含まない本文。
    pub message: String,
    pub task_id: Option<String>,
    pub task_origin: Option<String>,
    pub scheduled_at: Option<u64>,
}

impl ActivityRecord {
    /// 保存されている kind から表示用の 1 行を組み立てる。未知の kind (将来の版が書いた行) は
    /// そのまま `[kind]` を前置きして出す — 消すより読めた方がよい。
    pub(crate) fn display(&self) -> String {
        match parse_kind(&self.kind) {
            Some(kind) => match kind.prefix() {
                Some(prefix) => format!("{prefix} {}", self.message),
                None => self.message.clone(),
            },
            None => format!("[{}] {}", self.kind, self.message),
        }
    }
}

/// DB 上の識別子から [`ActivityKind`] を復元する。未知の値は `None`。
fn parse_kind(s: &str) -> Option<ActivityKind> {
    use ActivityKind::*;
    // 総当たりで突き合わせる。as_str() と対で保守されることを型で担保したいので、
    // 文字列リテラルをここに再掲しない。
    [
        Notify,
        Error,
        LoadError,
        InstantiateError,
        ScheduleError,
        Expanded,
        Rescheduled,
        Orphaned,
        Unavailable,
        Paused,
        Skipped,
        Expired,
        Unsupported,
        Manual,
        Deleted,
    ]
    .into_iter()
    .find(|k| k.as_str() == s)
}

/// 履歴の永続層。
///
/// **秘書を止めないことを優先する。** 書き込みに失敗しても stderr に出すだけで、心拍は
/// そのまま続ける (履歴が欠けるより秘書が動かなくなる方が実害が大きい)。
pub(crate) struct HistoryStore {
    conn: Connection,
}

impl HistoryStore {
    pub(crate) fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> rusqlite::Result<Self> {
        // WAL: 書き手 (worker スレッド) と読み手 (UI コマンド) が別スレッドで動く。
        // synchronous=NORMAL は WAL と組にすると「OS クラッシュで直近の数行を失うことは
        // あるが破損はしない」水準で、心拍ごとの fsync を避けられる。履歴の欠落は
        // 通知の欠落より軽い。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS activity (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts           INTEGER NOT NULL,
                 source       TEXT    NOT NULL,
                 kind         TEXT    NOT NULL,
                 message      TEXT    NOT NULL,
                 task_id      TEXT,
                 task_origin  TEXT,
                 scheduled_at INTEGER
             );
             -- 主クエリは「秘書が何をしたか」の全体時系列 (#6)。trigger 別は従なので
             -- ts の索引を主に張る (Mastra が trigger 単位で引くのとは主従が逆)。
             CREATE INDEX IF NOT EXISTS activity_ts ON activity(ts);
             CREATE INDEX IF NOT EXISTS activity_source_ts ON activity(source, ts);",
        )?;
        Ok(Self { conn })
    }

    /// 1 件追記する。戻り値は行 id。
    ///
    /// **id を返すのは live emit と保存済み履歴の突き合わせのため。** UI は起動時に両方を
    /// 受け取るので、同じイベントを 2 回並べないための同一判定が要る。`ts + source + message`
    /// では、同じ心拍で同じトリガーが同じ本文を 2 回出した場合 (スリープ復帰で ad-hoc と
    /// schedule 由来が両方走る等) に別のイベントを 1 つに潰してしまう。
    pub(crate) fn append(&self, ts: u64, activity: &Activity) -> rusqlite::Result<i64> {
        let task = activity.task.as_ref();
        self.conn.execute(
            "INSERT INTO activity (ts, source, kind, message, task_id, task_origin, scheduled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ts as i64,
                activity.source,
                activity.kind.as_str(),
                activity.message,
                task.map(|t| t.id.as_str()),
                task.map(|t| origin_str(t.origin)),
                task.map(|t| t.scheduled_at as i64),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 直近 `limit` 件を**新しい順**で返す。UI の初期表示はこれを読む。
    pub(crate) fn recent(&self, limit: usize) -> rusqlite::Result<Vec<ActivityRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, source, kind, message, task_id, task_origin, scheduled_at
             FROM activity ORDER BY ts DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(ActivityRecord {
                id: row.get(0)?,
                ts: row.get::<_, i64>(1)? as u64,
                source: row.get(2)?,
                kind: row.get(3)?,
                message: row.get(4)?,
                task_id: row.get(5)?,
                task_origin: row.get(6)?,
                scheduled_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
            })
        })?;
        rows.collect()
    }

    /// retention を適用する。戻り値は消した行数。
    ///
    /// 期間と件数の両方を見る。日数だけだと短い間隔のトリガーで膨らみ、件数だけだと
    /// 「先週何があったか」がトリガー数次第で見えなくなる。
    pub(crate) fn sweep(
        &self,
        now: u64,
        retention: Duration,
        max_rows: usize,
    ) -> rusqlite::Result<usize> {
        let cutoff = now.saturating_sub(retention.as_millis() as u64);
        let mut removed = self
            .conn
            .execute("DELETE FROM activity WHERE ts < ?1", [cutoff as i64])?;
        // 件数上限は id の新しい方から数えて残す。同一 ts が並んでも決定的に決まる。
        removed += self.conn.execute(
            "DELETE FROM activity WHERE id NOT IN (
                 SELECT id FROM activity ORDER BY id DESC LIMIT ?1
             )",
            [max_rows as i64],
        )?;
        Ok(removed)
    }

    #[cfg(test)]
    fn count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM activity", [], |r| r.get::<_, i64>(0))
            .unwrap() as usize
    }
}

/// `TaskOrigin` の永続表現。`TaskListItem::origin` (UI) と同じ文字列を使う。
pub(crate) fn origin_str(origin: TaskOrigin) -> &'static str {
    match origin {
        TaskOrigin::Schedule => "schedule",
        TaskOrigin::Adhoc => "adhoc",
    }
}

/// 掃除を走らせるべきか。心拍ごとに呼ばれるので比較 1 つで済ませる
/// ([`crate::tasks::needs_expansion`] と同じ閾値条件の作り)。
pub(crate) fn needs_sweep(last_sweep_at: u64, now: u64) -> bool {
    now.saturating_sub(last_sweep_at) >= SWEEP_INTERVAL.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 60 * 60 * 1000;
    const DAY: u64 = 24 * HOUR;

    /// 2026-01-01T00:00:00Z
    fn base() -> u64 {
        1_767_225_600_000
    }

    fn task(id: &str, at: u64) -> Task {
        Task {
            id: id.to_string(),
            origin: TaskOrigin::Schedule,
            trigger_id: Some("t".into()),
            scheduled_at: at,
            created_at: at,
        }
    }

    fn store() -> HistoryStore {
        HistoryStore::in_memory().unwrap()
    }

    #[test]
    fn append_and_read_back() {
        let s = store();
        s.append(
            base(),
            &Activity::new("t", ActivityKind::Notify, "おはようございます".into()),
        )
        .unwrap();

        let rows = s.recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, base());
        assert_eq!(rows[0].source, "t");
        assert_eq!(rows[0].kind, "notify");
        assert_eq!(rows[0].message, "おはようございます");
        assert!(rows[0].task_id.is_none());
    }

    /// タスク由来のイベントはスナップショットを持つ。タスクは実行後に消えるので
    /// 外部キーにはできない。
    #[test]
    fn task_snapshot_is_stored() {
        let s = store();
        s.append(
            base() + 5000,
            &Activity::new("t", ActivityKind::Skipped, "遅れています".into())
                .with_task(&task("t@1", base())),
        )
        .unwrap();

        let row = &s.recent(1).unwrap()[0];
        assert_eq!(row.task_id.as_deref(), Some("t@1"));
        assert_eq!(row.task_origin.as_deref(), Some("schedule"));
        assert_eq!(row.scheduled_at, Some(base()));
        // 遅延は ts と scheduled_at の差として導出する (フィールドに持たない)。
        assert_eq!(row.ts - row.scheduled_at.unwrap(), 5000);
    }

    /// 新しい順で返る。UI は先頭から並べるだけでよい。
    #[test]
    fn recent_returns_newest_first_and_honours_limit() {
        let s = store();
        for i in 0..5u64 {
            s.append(
                base() + i * HOUR,
                &Activity::new("t", ActivityKind::Notify, format!("{i}")),
            )
            .unwrap();
        }

        let rows = s.recent(3).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.message.as_str()).collect::<Vec<_>>(),
            vec!["4", "3", "2"]
        );
    }

    /// 内容が完全に同じ 2 件でも別の行として残り、id で区別できる。
    ///
    /// UI は live emit と保存済み履歴を突き合わせるので、ここが潰れると**イベントが
    /// 1 件消える**。スリープ復帰で ad-hoc と schedule 由来が同じ心拍で走り、同じ本文を
    /// 返す場合に実際に起きうる。
    #[test]
    fn identical_events_stay_distinguishable_by_id() {
        let s = store();
        let same = Activity::new("t", ActivityKind::Notify, "おはようございます".into());

        let first = s.append(base(), &same).unwrap();
        let second = s.append(base(), &same).unwrap();

        assert_ne!(first, second);
        let ids: Vec<i64> = s.recent(10).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![second, first]);
    }

    /// 同一 ts でも挿入順の逆で決定的に並ぶ (同じ心拍で複数イベントが出る)。
    #[test]
    fn same_timestamp_is_ordered_deterministically() {
        let s = store();
        for i in 0..3u64 {
            s.append(
                base(),
                &Activity::new("t", ActivityKind::Notify, format!("{i}")),
            )
            .unwrap();
        }

        let rows = s.recent(10).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.message.as_str()).collect::<Vec<_>>(),
            vec!["2", "1", "0"]
        );
    }

    // ---- retention ----------------------------------------------------------

    #[test]
    fn sweep_drops_rows_older_than_retention() {
        let s = store();
        s.append(
            base() - 31 * DAY,
            &Activity::new("t", ActivityKind::Notify, "古い".into()),
        )
        .unwrap();
        s.append(
            base() - 29 * DAY,
            &Activity::new("t", ActivityKind::Notify, "まだ残る".into()),
        )
        .unwrap();

        let removed = s.sweep(base(), RETENTION, MAX_ROWS).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(
            s.recent(10)
                .unwrap()
                .iter()
                .map(|r| r.message.as_str())
                .collect::<Vec<_>>(),
            vec!["まだ残る"]
        );
    }

    /// 期間内でも件数上限を超えたら古い方から落ちる。`@every 5m` を入れた環境で
    /// 日数だけでは止まらないため。
    #[test]
    fn sweep_enforces_the_row_cap() {
        let s = store();
        for i in 0..10u64 {
            s.append(
                base() + i,
                &Activity::new("t", ActivityKind::Expanded, format!("{i}")),
            )
            .unwrap();
        }

        let removed = s.sweep(base() + 100, RETENTION, 4).unwrap();

        assert_eq!(removed, 6);
        assert_eq!(s.count(), 4);
        assert_eq!(
            s.recent(10)
                .unwrap()
                .iter()
                .map(|r| r.message.as_str())
                .collect::<Vec<_>>(),
            vec!["9", "8", "7", "6"]
        );
    }

    #[test]
    fn sweep_is_a_noop_when_nothing_is_stale() {
        let s = store();
        s.append(
            base(),
            &Activity::new("t", ActivityKind::Notify, "新しい".into()),
        )
        .unwrap();

        assert_eq!(s.sweep(base(), RETENTION, MAX_ROWS).unwrap(), 0);
        assert_eq!(s.count(), 1);
    }

    /// ファイルに開いた DB がプロセスをまたいで読める。**これが #42 の眼目** —
    /// 「起動時に emit されて誰にも見えなかったイベント」が次の起動で読めること。
    #[test]
    fn rows_survive_reopening_the_file() {
        let dir = std::env::temp_dir().join(format!("chamberlain-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        let _ = std::fs::remove_file(&path);

        {
            let s = HistoryStore::open(&path).unwrap();
            s.append(
                base(),
                &Activity::new(
                    "broken",
                    ActivityKind::ScheduleError,
                    "unknown keyword: @hourl".into(),
                ),
            )
            .unwrap();
        }

        let reopened = HistoryStore::open(&path).unwrap();
        let rows = reopened.recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "broken");
        assert_eq!(
            rows[0].display(),
            "[schedule error] unknown keyword: @hourl"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn needs_sweep_uses_a_threshold() {
        assert!(!needs_sweep(base(), base() + 59 * 60 * 1000));
        assert!(needs_sweep(base(), base() + HOUR));
        // 起動直後 (last_sweep_at = 0) は必ず走る。
        assert!(needs_sweep(0, base()));
    }

    // ---- kind の表示 ---------------------------------------------------------

    /// 表示用プレフィックスは kind から組み立てる。文字列とのマッピングを二重に
    /// 持たないため。
    #[test]
    fn display_renders_the_prefix_from_kind() {
        let skipped = Activity::new("t", ActivityKind::Skipped, "遅れています".into());
        assert_eq!(skipped.display(), "[skipped] 遅れています");

        // 通知本文だけは前置きしない (エンドユーザーが読むのは秘書の言葉)。
        let notify = Activity::new("t", ActivityKind::Notify, "おはようございます".into());
        assert_eq!(notify.display(), "おはようございます");
    }

    /// 保存された行からも同じ表示が出る。
    #[test]
    fn stored_rows_render_the_same_way() {
        let s = store();
        s.append(
            base(),
            &Activity::new("t", ActivityKind::Paused, "破棄しました".into()),
        )
        .unwrap();

        assert_eq!(s.recent(1).unwrap()[0].display(), "[paused] 破棄しました");
    }

    /// 将来の版が書いた未知の kind でも行は読めるし表示もできる。retention 30 日の
    /// 移行期間中に古い / 新しい行が混ざるため (Mastra の legacy outcome と同じ問題)。
    #[test]
    fn unknown_kind_is_still_readable() {
        let s = store();
        s.conn
            .execute(
                "INSERT INTO activity (ts, source, kind, message) VALUES (?1, 't', 'suggested', '提案しました')",
                [base() as i64],
            )
            .unwrap();

        let row = &s.recent(1).unwrap()[0];
        assert_eq!(row.kind, "suggested");
        assert_eq!(row.display(), "[suggested] 提案しました");
    }

    #[test]
    fn kind_identifiers_round_trip() {
        for kind in [
            ActivityKind::Notify,
            ActivityKind::Error,
            ActivityKind::LoadError,
            ActivityKind::InstantiateError,
            ActivityKind::ScheduleError,
            ActivityKind::Expanded,
            ActivityKind::Rescheduled,
            ActivityKind::Orphaned,
            ActivityKind::Unavailable,
            ActivityKind::Paused,
            ActivityKind::Skipped,
            ActivityKind::Expired,
            ActivityKind::Unsupported,
            ActivityKind::Manual,
            ActivityKind::Deleted,
        ] {
            assert_eq!(parse_kind(kind.as_str()), Some(kind));
        }
        assert_eq!(parse_kind("nope"), None);
    }
}
