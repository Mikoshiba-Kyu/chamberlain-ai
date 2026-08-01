//! 心拍の配線層 (#46)。
//!
//! [`crate::tasks`] が「due なタスク 1 件をどう扱うか」の純粋な判断を持つのに対し、この
//! モジュールは**その判断を実際の副作用に繋ぐ順序**を持つ。展開 → due 取り出し → 分類 →
//! 実行 → 後片付け、という一巡そのものがここの責務である。
//!
//! # なぜ trait を挟むか
//!
//! 0.2.0 まで、この一巡は `lib.rs` の worker スレッド内に直に書かれていて `AppHandle` と
//! `rustyscript::Runtime` に直接依存していた。結果として **配線層のテストが 1 本も書けず**、
//! #26 Phase 1 の検証は全て実機で手作業になった (#46)。
//!
//! [`WorkerHost`] は worker が触る副作用を 6 つに数え上げたものである。本番の実装は
//! `lib.rs` の `TauriHost` (AppHandle + JS Runtime + ロード済みモジュール)、テストでは
//! 記録用の fake を差す。これで「時計を進めるだけでシナリオを書く」ことができる。
//!
//! JS 実行 ([`WorkerHost::call_tick`]) も境界の裏に置いた。V8 は thread affinity を持ち
//! 起動も安くないので、心拍のロジックを検証するのに本物の Runtime を回す必要は無い。
//! 「トリガーの JS が実際に動くか」は別レイヤの関心であり、`rustyscript` 自体の責務に近い。
//!
//! # 副作用の順序は仕様である
//!
//! トリガー 1 件の実行順序は state読 → tick(ctx) → notify → state保存 → タスク削除。
//! - notify が state 保存より先: プロセスクラッシュ時の "at least once" を優先
//!   (秘書は「1 回多く言う > 一言忘れる」)
//! - タスク削除が最後: 同じ理由。実行中にクラッシュしたタスクはリストに残り、次回起動で
//!   もう一度試される
//! - tick() がエラーを返してもタスクは消す。schedule の意味を「実行を試みる時刻」に統一し、
//!   エラーで毎心拍リトライになるノイズを避ける (0.1.x の fire_time 更新方針を踏襲)
//!
//! この順序はテストで列として固定されている (`side_effects_happen_in_the_documented_order` /
//! `crash_before_cleanup_keeps_the_task`)。

use std::sync::Mutex;
use std::time::Duration;

use chrono_tz::Tz;
use serde::Deserialize;

use crate::history::{needs_sweep, Activity, ActivityKind};
use crate::schedule::Schedule;
use crate::tasks::{
    classify_due, expand_trigger, needs_expansion, reconcile, sweep_stale, Disposition,
    ExpansionState, TaskStore, TriggerRuntimeState, TriggerSpecView,
};

#[derive(Deserialize, Clone)]
pub(crate) struct NotifyPayload {
    /// 省略時は notify 側で manifest.name をデフォルトに使う
    #[serde(default)]
    pub title: Option<String>,
    pub body: String,
}

#[derive(Deserialize, Default, Clone)]
pub(crate) struct TickResult {
    #[serde(default)]
    pub notify: Option<NotifyPayload>,
    #[serde(default)]
    pub state: Option<serde_json::Value>,
}

/// worker が触る副作用の全て。ここに無い副作用を worker が持ってはならない
/// (持った瞬間にその経路がテストの外に出る)。
pub(crate) trait WorkerHost {
    /// トリガーの永続 state (`triggers-state.json`) を読む。未設定なら `{}`。
    fn read_state(&mut self, trigger_id: &str) -> serde_json::Value;

    /// トリガーの永続 state を書く。
    fn write_state(&mut self, trigger_id: &str, state: serde_json::Value);

    /// トリガーの JS `tick(ctx)` を呼ぶ。`Ok(None)` は「戻り値なし」。
    /// `Err` は JS 例外・ロード済みモジュール不在などの実行失敗。
    fn call_tick(
        &mut self,
        trigger_id: &str,
        ctx: serde_json::Value,
    ) -> Result<Option<TickResult>, String>;

    /// OS 通知を出す。permission が無い等で出せない場合も worker には成功として見せる
    /// (観測面は activity 側が担保するため)。
    fn notify(&mut self, title: &str, body: &str);

    /// 観測面 (#6) にイベントを流す。live emit と履歴への永続化の両方がここに入る (#42)。
    fn activity(&mut self, activity: Activity);

    /// 履歴の retention を適用する。戻り値は消した行数 (観測用)。
    fn sweep_history(&mut self, now: u64) -> usize;

    /// タスクリストを永続化する。1 心拍あたり最大 1 回に抑えるのは worker の責任。
    fn save_tasks(&mut self, store: &TaskStore);
}

/// 心拍から見たトリガー 1 つ。`lib.rs` の `TriggerInfo` (PathBuf や AtomicBool を持つ)
/// から心拍が必要とするぶんだけを写し取ったもの。
///
/// 所有型にしてあるのは、`paused` が心拍ごとに読み直される値だからである。参照で持つと
/// 「いつの時点の paused か」がライフタイムに紛れる。トリガーは数個の規模なので、
/// 心拍ごとに数本の String を clone するコストは無視できる。
#[derive(Clone, Debug)]
pub(crate) struct TriggerSpec {
    pub id: String,
    /// 通知タイトルのデフォルト。
    pub name: String,
    /// manifest の生の schedule 文字列。変更検知に使う (パース結果ではなく文字列を比較)。
    pub schedule_raw: String,
    pub tz_raw: Option<String>,
    /// パース済み schedule。展開器の生成規則。`runnable` が false のときは参照されない。
    pub schedule: Schedule,
    pub tz: Tz,
    pub paused: bool,
    /// 「今このトリガーを実行できるか」。構成エラー (schedule / tz) と JS ロード失敗の
    /// どちらも false になる。false のトリガーは展開されず、既に積まれたタスクは
    /// due 時に `[unavailable]` で破棄される。
    pub runnable: bool,
}

fn find_spec<'a>(specs: &'a [TriggerSpec], id: &str) -> Option<&'a TriggerSpec> {
    specs.iter().find(|s| s.id == id)
}

/// 心拍をまたいで持ち越す worker の状態。
///
/// タスクリストと違って永続化しない。落としても「次の心拍で 1 回余分に掃除が走る」だけで、
/// 安全側に倒れる値しか置かない。
#[derive(Default)]
pub(crate) struct WorkerState {
    /// 最後に履歴の retention を適用した時刻。0 は「まだ一度も」= 起動直後に 1 回走る。
    last_sweep_at: u64,
}

/// タスクリストを掴む。poison は「前回の panic の残骸」として無視して中身を取り出す。
///
/// 通常なら poison は異常の伝播として尊重すべきだが、ここでは 1 度の panic 以降
/// タスクリストが永久に触れなくなる (= 秘書が二度と動かない) 方が実害が大きい。
pub(crate) fn lock_tasks(store: &Mutex<TaskStore>) -> std::sync::MutexGuard<'_, TaskStore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// 起動時にタスクリストを現在の manifest と突き合わせ、結果を観測面に流す。
///
/// 突き合わせには **discovery で見えている全トリガー** を渡す (`runnable` が false の
/// ものも含む)。ロード失敗は一時的なこともあり、それだけで展開済み境界や積まれたタスクを
/// 破棄すると「1 回のビルド事故でスケジュールの記憶が消える」ことになる。実行可否は
/// [`expand_pending`] と due 判定側が別途見る。
pub(crate) fn reconcile_at_startup<H: WorkerHost>(
    host: &mut H,
    specs: &[TriggerSpec],
    task_store: &Mutex<TaskStore>,
    now: u64,
) {
    let views: Vec<TriggerSpecView<'_>> = specs
        .iter()
        .map(|s| TriggerSpecView {
            id: &s.id,
            schedule: &s.schedule_raw,
            tz: s.tz_raw.as_deref(),
        })
        .collect();

    let report = {
        let mut store = lock_tasks(task_store);
        reconcile(&mut store, &views, now)
    };

    for (task_id, trigger_id) in &report.orphaned {
        eprintln!("dropped task '{task_id}': trigger '{trigger_id}' no longer exists");
        host.activity(Activity::new(
            trigger_id,
            ActivityKind::Orphaned,
            format!("トリガーが存在しないため予定を破棄しました ({task_id})"),
        ));
    }
    for trigger_id in &report.rescheduled {
        eprintln!("trigger '{trigger_id}': schedule changed, re-expanding");
        host.activity(Activity::new(
            trigger_id,
            ActivityKind::Rescheduled,
            "schedule が変更されたため未実行の予定を破棄して再展開します".to_string(),
        ));
    }

    let store = lock_tasks(task_store);
    host.save_tasks(&store);
}

/// 閾値条件を満たしたトリガーを展開する (#26 決定事項 6)。
///
/// 展開対象は実行可能なトリガーだけ。構成エラーのあるトリガーや JS のロードに失敗した
/// トリガーを展開すると、実行できないタスクを積んでから due 時に破棄するだけになる。
///
/// 戻り値はタスクリスト / 境界が変化したか (呼び出し側が save の要否を判断する)。
fn expand_pending<H: WorkerHost>(
    host: &mut H,
    specs: &[TriggerSpec],
    task_store: &Mutex<TaskStore>,
    now: u64,
) -> bool {
    // 観測面への emit をロックの外に出すため、展開結果を先に溜める。
    let mut expanded: Vec<(String, usize, u64)> = Vec::new();
    let mut dirty = false;

    {
        let mut store = lock_tasks(task_store);
        for spec in specs {
            if !spec.runnable {
                continue;
            }
            // 境界が無い = reconcile 直後に消えた等の異常時。now を起点にすれば過去は生成しない。
            let boundary = store
                .expansion
                .get(&spec.id)
                .map(|s| s.expanded_until)
                .unwrap_or(now);
            if !needs_expansion(boundary, now) {
                continue;
            }

            let (generated, new_boundary) =
                expand_trigger(&spec.id, &spec.schedule, &spec.tz, boundary, now);
            let added = store.extend(generated);

            // 境界は生成 0 件でも進める。`@at` のように規則が尽きたトリガーで毎心拍
            // 展開を再試行しないため。
            store
                .expansion
                .entry(spec.id.clone())
                .and_modify(|s| s.expanded_until = new_boundary)
                .or_insert_with(|| ExpansionState {
                    expanded_until: new_boundary,
                    schedule: spec.schedule_raw.clone(),
                    tz: spec.tz_raw.clone(),
                });
            dirty = true;

            if added > 0 {
                expanded.push((spec.id.clone(), added, new_boundary));
            }
        }
    }

    for (id, added, boundary) in expanded {
        host.activity(Activity::new(
            &id,
            ActivityKind::Expanded,
            format!("{added} 件のタスクを積みました (〜{})", fmt_utc(boundary)),
        ));
    }
    dirty
}

/// 猶予を超えた schedule 由来タスクを掃除し、結果を観測面に流す (#50 / #53)。
///
/// **1 件のときは畳まない。** 1 行にまとめても情報が増えず、代わりに「いつの予定が
/// 流れたか」が消えるため。心拍が 1 回遅れただけ、という日常的なケースはこちらに落ちる。
///
/// 戻り値はタスクリストが変化したか (呼び出し側が save の要否を判断する)。
fn sweep_stale_tasks<H: WorkerHost>(
    host: &mut H,
    task_store: &Mutex<TaskStore>,
    now: u64,
    schedule_grace: Duration,
) -> bool {
    let groups = {
        let mut store = lock_tasks(task_store);
        sweep_stale(&mut store, now, schedule_grace)
    };
    if groups.is_empty() {
        return false;
    }

    for group in groups {
        if let [task] = group.tasks.as_slice() {
            host.activity(
                Activity::new(
                    &group.trigger_id,
                    ActivityKind::Skipped,
                    format!(
                        "{} の予定に {} 遅れているため実行しませんでした",
                        fmt_utc(task.scheduled_at),
                        fmt_delay(now.saturating_sub(task.scheduled_at))
                    ),
                )
                .with_task(task),
            );
            continue;
        }
        // 先頭が最も古い (タスクリストは scheduled_at 昇順)。
        let oldest = group.tasks[0].scheduled_at;
        eprintln!(
            "trigger '{}': dropped {} stale task(s), oldest {}",
            group.trigger_id,
            group.tasks.len(),
            fmt_utc(oldest)
        );
        host.activity(Activity::new(
            &group.trigger_id,
            ActivityKind::Stale,
            format!(
                "{} 件の予定を遅延で破棄しました (最古 {})",
                group.tasks.len(),
                fmt_utc(oldest)
            ),
        ));
    }
    true
}

/// 心拍 1 回。展開 → due 取り出し → 分類 → 実行 → 後片付け。
///
/// `now` を引数に取るのは意図的で、テストが時計を進めるだけでシナリオを書けるようにする
/// ためであると同時に、**同一心拍内の全タスクが同じ `now` を見る**ことの保証でもある
/// (1 件の tick() に時間がかかっても、後続タスクの遅延判定がそのぶんずれない)。
pub(crate) fn heartbeat<H: WorkerHost>(
    host: &mut H,
    specs: &[TriggerSpec],
    task_store: &Mutex<TaskStore>,
    state: &mut WorkerState,
    now: u64,
    schedule_grace: Duration,
) {
    // 0. 履歴の retention (閾値条件。#26 決定事項 6 と同じ理由で時刻イベントにしない)。
    //    実行より先に置くのは、この心拍が積む行を消させないため。
    if needs_sweep(state.last_sweep_at, now) {
        let removed = host.sweep_history(now);
        state.last_sweep_at = now;
        if removed > 0 {
            eprintln!("history: swept {removed} row(s)");
        }
    }

    // 1. 実行できないものの掃除。展開より先に置くのは、この心拍で積むタスクを
    //    掃除の対象にしないため (展開器は未来しか生成しないので実害は無いが、
    //    「積む前に片付ける」の方が読みやすい)。
    let swept = sweep_stale_tasks(host, task_store, now, schedule_grace);

    // 2. 展開 (閾値条件)。
    let dirty = swept | expand_pending(host, specs, task_store, now);

    // 3. due 取り出し。ロックは掴んだまま JS を回さない (list_tasks / delete_task が
    //    tick() の実行時間ぶん待たされるのを避ける)。
    let due = lock_tasks(task_store).due(now);

    // 4. 分類と実行。
    let mut handled: Vec<String> = Vec::new();
    for task in due {
        let spec = task
            .trigger_id
            .as_deref()
            .and_then(|tid| find_spec(specs, tid));
        // 実行可能なのは「discovery で見えていて、かつ実行可能な状態」のときだけ。
        // どちらかを欠く場合は classify_due に None を渡して破棄させる。
        let runtime_state = spec
            .filter(|s| s.runnable)
            .map(|s| TriggerRuntimeState { paused: s.paused });

        let source = task.trigger_id.clone().unwrap_or_else(|| "__task__".into());
        match classify_due(&task, now, runtime_state, schedule_grace) {
            Disposition::Run { delay_ms } => {
                // classify_due が Run を返した = spec が実行可能な状態で揃っている。
                let Some(spec) = spec else {
                    // trigger_id を持たないタスク (Phase 3 の自然言語タスク) は
                    // Phase 1 では実行経路が無い。積まれたら破棄して痕跡を残す。
                    host.activity(
                        Activity::new(
                            &source,
                            ActivityKind::Unsupported,
                            "実行対象のトリガーが無いタスクは Phase 1 では実行できません"
                                .to_string(),
                        )
                        .with_task(&task),
                    );
                    handled.push(task.id.clone());
                    continue;
                };
                let id = spec.id.as_str();
                let current_state = host.read_state(id);
                // scheduledAt / delayMs を渡すのは追加決定 11。遅延をどう伝えるかは
                // framework が本文に前置きするのではなく、トリガー側に判断させる。
                let ctx = serde_json::json!({
                    "now": now,
                    "state": current_state,
                    "scheduledAt": task.scheduled_at,
                    "delayMs": delay_ms,
                });
                match host.call_tick(id, ctx) {
                    Ok(Some(res)) => {
                        if let Some(notify) = res.notify {
                            let title = notify.title.unwrap_or_else(|| spec.name.clone());
                            host.notify(&title, &notify.body);
                            // 通知は必ず観測面にも出す (#6)。OS 通知の描画に依存せず
                            // 「秘書が何を言ったか」を UI から追えることが要件。
                            host.activity(
                                Activity::new(id, ActivityKind::Notify, notify.body)
                                    .with_task(&task),
                            );
                        }
                        if let Some(new_state) = res.state {
                            host.write_state(id, new_state);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("trigger '{id}' tick() error: {e}");
                        host.activity(Activity::new(id, ActivityKind::Error, e).with_task(&task));
                    }
                }
            }
            Disposition::Orphaned => {
                // discovery では見えているのに実行できない場合と、manifest から
                // 消えている場合を言い分ける (原因の切り分けが変わるため)。
                let (kind, message) = if spec.is_some() {
                    (
                        ActivityKind::Unavailable,
                        "トリガーが実行できない状態 (構成エラー / ロード失敗) \
                         のため予定を破棄しました"
                            .to_string(),
                    )
                } else {
                    (
                        ActivityKind::Orphaned,
                        "トリガーが存在しないため予定を破棄しました".to_string(),
                    )
                };
                host.activity(Activity::new(&source, kind, message).with_task(&task));
            }
            Disposition::Paused => {
                host.activity(
                    Activity::new(
                        &source,
                        ActivityKind::Paused,
                        format!(
                            "停止中のため {} の予定を破棄しました",
                            fmt_utc(task.scheduled_at)
                        ),
                    )
                    .with_task(&task),
                );
            }
            // ここには来ない。猶予超過は step 1 の掃除が先に拾う (#53)。
            // 掃除と `classify_due` の判定がずれた場合の安全網として残してある —
            // 黙って実行してしまうより、痕跡を残して破棄する方がよい。
            Disposition::SkippedLate { delay_ms } => {
                host.activity(
                    Activity::new(
                        &source,
                        ActivityKind::Skipped,
                        format!(
                            "{} の予定に {} 遅れているため実行しませんでした",
                            fmt_utc(task.scheduled_at),
                            fmt_delay(delay_ms)
                        ),
                    )
                    .with_task(&task),
                );
            }
            Disposition::Expired { delay_ms } => {
                host.activity(
                    Activity::new(
                        &source,
                        ActivityKind::Expired,
                        format!(
                            "{} の予定が猶予を超えた ({} 遅れ) ため未実行のまま破棄しました",
                            fmt_utc(task.scheduled_at),
                            fmt_delay(delay_ms)
                        ),
                    )
                    .with_task(&task),
                );
            }
        }
        handled.push(task.id.clone());
    }

    // 5. 後片付け。実行後に消すことで at-least-once を保つ。
    //    展開だけがあった心拍でも境界が動いているので save は必要。
    if !handled.is_empty() || dirty {
        let mut store = lock_tasks(task_store);
        for id in &handled {
            store.remove(id);
        }
        host.save_tasks(&store);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::parse_schedule;
    use crate::tasks::{Task, TaskOrigin};
    use std::collections::BTreeMap;

    const MINUTE: u64 = 60 * 1000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    /// 心拍 1 分 × 猶予 2 回分 (prod 相当)。
    fn grace() -> Duration {
        Duration::from_secs(120)
    }

    /// 2026-01-01T00:00:00Z (UTC の日付境界ちょうど)。
    fn base() -> u64 {
        1_767_225_600_000
    }

    /// fake が記録した副作用 1 件。**順序が仕様である**ため、種類ごとの配列ではなく
    /// 1 本の列に混ぜて記録する。
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Effect {
        ReadState(String),
        WriteState(String, String),
        Tick(String),
        Notify(String, String),
        /// (source, kind, 表示用の 1 行)。kind を持つのは #42 で列に出したため。
        Activity(String, &'static str, String),
        Sweep,
        Save(usize),
    }

    /// テスト用の [`WorkerHost`]。JS は「トリガー id → 返す結果」の表で代用する。
    #[derive(Default)]
    struct FakeHost {
        effects: Vec<Effect>,
        /// tick() の戻り値。未登録のトリガーは `Ok(None)` を返す。
        replies: BTreeMap<String, Result<Option<TickResult>, String>>,
        /// トリガーの永続 state。
        states: BTreeMap<String, serde_json::Value>,
        /// この id の write_state で panic する (クラッシュの模擬)。
        panic_on_write_state: Option<String>,
        /// 最後に永続化されたタスク id の一覧。
        saved: Vec<String>,
        saves: usize,
        /// 流れた activity をそのまま保持する (kind / task ref の検証用)。
        activities: Vec<Activity>,
        sweeps: usize,
    }

    impl FakeHost {
        fn reply(mut self, trigger_id: &str, result: Result<Option<TickResult>, String>) -> Self {
            self.replies.insert(trigger_id.to_string(), result);
            self
        }

        /// 記録された副作用のうち activity の (source, 表示行) だけを取り出す。
        fn activities(&self) -> Vec<(&str, String)> {
            self.activities
                .iter()
                .map(|a| (a.source.as_str(), a.display()))
                .collect()
        }

        /// 指定 kind の activity が何件出たか。
        fn count_kind(&self, kind: ActivityKind) -> usize {
            self.activities.iter().filter(|a| a.kind == kind).count()
        }

        /// 指定 kind の activity を 1 件だけ取る (複数あれば panic)。
        fn only(&self, kind: ActivityKind) -> &Activity {
            let mut found = self.activities.iter().filter(|a| a.kind == kind);
            let first = found
                .next()
                .unwrap_or_else(|| panic!("no {kind:?} activity"));
            assert!(found.next().is_none(), "{kind:?} が複数出ている");
            first
        }
    }

    impl WorkerHost for FakeHost {
        fn read_state(&mut self, trigger_id: &str) -> serde_json::Value {
            self.effects.push(Effect::ReadState(trigger_id.into()));
            self.states
                .get(trigger_id)
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}))
        }

        fn write_state(&mut self, trigger_id: &str, state: serde_json::Value) {
            if self.panic_on_write_state.as_deref() == Some(trigger_id) {
                panic!("simulated crash while persisting state for {trigger_id}");
            }
            self.effects
                .push(Effect::WriteState(trigger_id.into(), state.to_string()));
            self.states.insert(trigger_id.into(), state);
        }

        fn call_tick(
            &mut self,
            trigger_id: &str,
            _ctx: serde_json::Value,
        ) -> Result<Option<TickResult>, String> {
            self.effects.push(Effect::Tick(trigger_id.into()));
            // 同じトリガーが同一心拍で複数回 due になることがあるので、返答は消費しない。
            match self.replies.get(trigger_id) {
                Some(Ok(r)) => Ok(r.clone()),
                Some(Err(e)) => Err(e.clone()),
                None => Ok(None),
            }
        }

        fn notify(&mut self, title: &str, body: &str) {
            self.effects.push(Effect::Notify(title.into(), body.into()));
        }

        fn activity(&mut self, activity: Activity) {
            self.activities.push(activity.clone());
            self.effects.push(Effect::Activity(
                activity.source.clone(),
                activity.kind.as_str(),
                activity.display(),
            ));
        }

        fn sweep_history(&mut self, _now: u64) -> usize {
            self.effects.push(Effect::Sweep);
            self.sweeps += 1;
            0
        }

        fn save_tasks(&mut self, store: &TaskStore) {
            self.saves += 1;
            self.saved = store.tasks.iter().map(|t| t.id.clone()).collect();
            self.effects.push(Effect::Save(store.tasks.len()));
        }
    }

    /// テスト用のトリガー定義。デフォルトは「実行可能・停止していない」。
    fn spec(id: &str, schedule: &str) -> TriggerSpec {
        TriggerSpec {
            id: id.to_string(),
            name: format!("{id} trigger"),
            schedule_raw: schedule.to_string(),
            tz_raw: Some("UTC".to_string()),
            schedule: parse_schedule(schedule).expect("test schedule must parse"),
            tz: "UTC".parse().unwrap(),
            paused: false,
            runnable: true,
        }
    }

    fn store_of(tasks: Vec<Task>) -> Mutex<TaskStore> {
        let mut s = TaskStore {
            tasks,
            ..Default::default()
        };
        s.normalize();
        Mutex::new(s)
    }

    /// 展開を起こさない store。境界を先に置くことで、心拍のうち「実行」の部分だけを
    /// 見たいテストが `[expanded]` のノイズを踏まないようにする。
    fn store_without_expansion(trigger: &str, tasks: Vec<Task>) -> Mutex<TaskStore> {
        let store = store_of(tasks);
        lock_tasks(&store).expansion.insert(
            trigger.to_string(),
            ExpansionState {
                expanded_until: base() + 2 * DAY,
                schedule: "@hourly".into(),
                tz: Some("UTC".into()),
            },
        );
        store
    }

    fn adhoc(id: &str, trigger: &str, at: u64) -> Task {
        Task {
            id: id.to_string(),
            origin: TaskOrigin::Adhoc,
            trigger_id: Some(trigger.to_string()),
            scheduled_at: at,
            created_at: at,
        }
    }

    fn tick_result(body: &str) -> TickResult {
        TickResult {
            notify: Some(NotifyPayload {
                title: None,
                body: body.to_string(),
            }),
            state: None,
        }
    }

    fn ids(store: &Mutex<TaskStore>) -> Vec<String> {
        lock_tasks(store)
            .tasks
            .iter()
            .map(|t| t.id.clone())
            .collect()
    }

    fn boundary(store: &Mutex<TaskStore>, trigger: &str) -> Option<u64> {
        lock_tasks(store)
            .expansion
            .get(trigger)
            .map(|s| s.expanded_until)
    }

    // ---- 起動直後 ------------------------------------------------------------

    /// 空の tasks.json から起動すると、48h ぶんが積まれて境界が `now + 48h` になる。
    /// #26 Phase 1 の実機検証 (56 件生成 / 境界が全トリガーで同一) に対応する。
    #[test]
    fn first_tick_expands_the_full_horizon() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let specs = vec![spec("hourly", "@hourly"), spec("daily", "@daily 06:00")];
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        // @hourly は 48h ぶんで 48 件 (起点 00:00 は「境界より後」なので含まれない)。
        // @daily 06:00 は 2 件 (初日と翌日)。
        let tasks = lock_tasks(&store);
        assert_eq!(
            tasks
                .tasks
                .iter()
                .filter(|t| t.trigger_id.as_deref() == Some("hourly"))
                .count(),
            48
        );
        assert_eq!(
            tasks
                .tasks
                .iter()
                .filter(|t| t.trigger_id.as_deref() == Some("daily"))
                .count(),
            2
        );
        drop(tasks);

        // 境界は全トリガーで同一の now + 48h。
        assert_eq!(boundary(&store, "hourly"), Some(base() + 2 * DAY));
        assert_eq!(boundary(&store, "daily"), Some(base() + 2 * DAY));

        assert_eq!(host.count_kind(ActivityKind::Expanded), 2);
        // 展開だけの心拍でも境界が動いているので永続化される。
        assert_eq!(host.saves, 1);
    }

    /// 展開は wall-clock グリッドに載る。`@daily 06:00` は UTC の 06:00 ちょうど。
    #[test]
    fn expansion_lands_on_the_wall_clock_grid() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let specs = vec![spec("daily", "@daily 06:00")];
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        let tasks = lock_tasks(&store);
        let times: Vec<u64> = tasks.tasks.iter().map(|t| t.scheduled_at).collect();
        assert_eq!(times, vec![base() + 6 * HOUR, base() + DAY + 6 * HOUR]);
    }

    /// 閾値に達していないトリガーは再展開しない (毎心拍展開しない)。
    #[test]
    fn second_tick_does_not_re_expand() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let specs = vec![spec("hourly", "@hourly")];
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        let after_first = ids(&store);
        let saves_after_first = host.saves;

        // 1 分後。境界は now + 48h - 1m で、閾値 (now + 24h) にはまだ遠い。
        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + MINUTE,
            grace(),
        );

        assert_eq!(ids(&store), after_first);
        assert_eq!(host.count_kind(ActivityKind::Expanded), 1);
        // 展開も実行も無い心拍は書き込まない (write amplification を避ける)。
        assert_eq!(host.saves, saves_after_first);
    }

    // ---- 長期停止からの復帰 --------------------------------------------------

    /// 1 週間前の境界で起動しても過去のタスクは 1 件も生成しない (#26 「過去は生成しない」)。
    #[test]
    fn stale_boundary_does_not_generate_past_tasks() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let specs = vec![spec("hourly", "@hourly")];
        let store = store_of(vec![]);

        // 1 週間前に展開して、そこから 1 週間アプリを閉じていた状況を作る。
        let long_ago = base() - 7 * DAY;
        heartbeat(&mut host, &specs, &store, &mut state, long_ago, grace());
        // 積まれた過去タスクは検証対象ではないので捨てる (境界だけ残す)。
        lock_tasks(&store).tasks.clear();

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        let tasks = lock_tasks(&store);
        assert!(
            tasks.tasks.iter().all(|t| t.scheduled_at > base()),
            "過去のタスクが生成された: {:?}",
            tasks
                .tasks
                .iter()
                .map(|t| t.scheduled_at)
                .filter(|at| *at <= base())
                .collect::<Vec<_>>()
        );
    }

    // ---- スリープ復帰 --------------------------------------------------------

    /// 時計が 3 時間飛ぶと、schedule 由来は猶予超過で破棄され、ad-hoc は猶予 (24h) 内なので
    /// 実行される。#26 決定事項 8 の origin 別猶予。
    #[test]
    fn sleep_resume_skips_schedule_tasks_but_runs_adhoc() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply("hourly", Ok(Some(tick_result("起きました"))));
        let store = store_of(vec![]);

        // 00:00 に起動。01:00 / 02:00 / ... が積まれる。
        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        // スリープ前に手動実行を予約した想定の ad-hoc を 1 件積む。
        lock_tasks(&store).insert(adhoc("manual-hourly-1", "hourly", base() + MINUTE));

        // 3 時間スリープして 03:00 ちょうどに復帰。
        let resumed = base() + 3 * HOUR;
        heartbeat(&mut host, &specs, &store, &mut state, resumed, grace());

        // 01:00 (2h 遅れ) と 02:00 (1h 遅れ) が猶予 (2 分) を超えて破棄される。
        // 2 件あるので 1 行に畳まれる (#53)。03:00 は遅れ 0 なので実行される。
        assert_eq!(host.count_kind(ActivityKind::Stale), 1);
        assert_eq!(host.count_kind(ActivityKind::Skipped), 0);
        assert!(!ids(&store).contains(&format!("hourly@{}", base() + HOUR)));
        assert!(!ids(&store).contains(&format!("hourly@{}", base() + 2 * HOUR)));

        // 実行されたのは ad-hoc (3h 遅れだが猶予 24h 内) と 03:00 の 2 件。
        assert_eq!(
            host.effects
                .iter()
                .filter(|e| matches!(e, Effect::Tick(_)))
                .count(),
            2
        );
        assert!(host.effects.contains(&Effect::Notify(
            "hourly trigger".into(),
            "起きました".into()
        )));
        assert!(!ids(&store).contains(&"manual-hourly-1".to_string()));
    }

    /// 猶予内 (心拍 2 回分) の遅れは実行する。「心拍が拾い損ねた」ぶんは失わない。
    #[test]
    fn delay_within_grace_still_runs() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply("hourly", Ok(Some(tick_result("定時報告"))));
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        // 01:00 の予定を 90 秒遅れで拾う (猶予 120 秒以内)。
        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + HOUR + 90 * 1000,
            grace(),
        );

        assert_eq!(host.count_kind(ActivityKind::Skipped), 0);
        assert!(host
            .effects
            .contains(&Effect::Notify("hourly trigger".into(), "定時報告".into())));
    }

    // ---- pause -------------------------------------------------------------

    /// pause 中に due が来たタスクは破棄され、`[paused]` が記録される。
    /// 境界は進み続けるので、resume 後に「止めていた間の予定」が遡って積まれることはない。
    #[test]
    fn paused_trigger_drops_due_tasks_but_keeps_expanding() {
        let mut state = WorkerState::default();
        let mut specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default();
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        let boundary_before = boundary(&store, "hourly");

        specs[0].paused = true;
        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + HOUR,
            grace(),
        );

        assert_eq!(host.count_kind(ActivityKind::Paused), 1);
        assert!(!host.effects.iter().any(|e| matches!(e, Effect::Tick(_))));
        // 破棄されたので同じタスクが次の心拍でもう一度 due になることはない。
        assert!(!ids(&store).contains(&format!("hourly@{}", base() + HOUR)));
        // 境界はこの心拍では閾値未満なので動かない。止めても巻き戻らないことが要点。
        assert_eq!(boundary(&store, "hourly"), boundary_before);
    }

    // ---- schedule 変更 / 孤児掃除 -------------------------------------------

    /// schedule を変えて再起動すると、そのトリガーだけ再展開され、他は境界も予定も不変。
    #[test]
    fn schedule_change_re_expands_only_that_trigger() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let specs = vec![spec("hourly", "@hourly"), spec("daily", "@daily 06:00")];
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        let daily_before: Vec<String> = ids(&store)
            .into_iter()
            .filter(|id| id.starts_with("daily@"))
            .collect();
        let daily_boundary = boundary(&store, "daily");

        // 再起動: hourly の schedule だけ変わっている。
        let restarted = vec![spec("hourly", "@hourly :30"), spec("daily", "@daily 06:00")];
        let restart_at = base() + 10 * MINUTE;
        let mut host2 = FakeHost::default();
        reconcile_at_startup(&mut host2, &restarted, &store, restart_at);

        let rescheduled = host2.only(ActivityKind::Rescheduled);
        assert_eq!(rescheduled.source, "hourly");
        assert_eq!(
            rescheduled.display(),
            "[rescheduled] schedule が変更されたため未実行の予定を破棄して再展開します"
        );
        // 変更されたトリガーの予定は消え、境界は now に戻る。
        assert!(!ids(&store).iter().any(|id| id.starts_with("hourly@")));
        assert_eq!(boundary(&store, "hourly"), Some(restart_at));
        // 他のトリガーは無傷。
        assert_eq!(
            ids(&store)
                .into_iter()
                .filter(|id| id.starts_with("daily@"))
                .collect::<Vec<_>>(),
            daily_before
        );
        assert_eq!(boundary(&store, "daily"), daily_boundary);

        // 次の心拍で :30 グリッドに載り直す。
        heartbeat(
            &mut host2,
            &restarted,
            &store,
            &mut state,
            restart_at,
            grace(),
        );
        let first_hourly = lock_tasks(&store)
            .tasks
            .iter()
            .filter(|t| t.trigger_id.as_deref() == Some("hourly"))
            .map(|t| t.scheduled_at)
            .min();
        assert_eq!(first_hourly, Some(base() + 30 * MINUTE));
    }

    /// トリガーを消して再起動すると、そのトリガーを指すタスクと展開状態が破棄される。
    #[test]
    fn removed_trigger_is_reaped_at_startup() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let specs = vec![spec("hourly", "@hourly"), spec("daily", "@daily 06:00")];
        let store = store_of(vec![]);
        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        // daily が manifest から消えた状態で再起動。
        let restarted = vec![spec("hourly", "@hourly")];
        let mut host2 = FakeHost::default();
        reconcile_at_startup(&mut host2, &restarted, &store, base() + MINUTE);

        assert!(!ids(&store).iter().any(|id| id.starts_with("daily@")));
        assert_eq!(boundary(&store, "daily"), None);
        assert_eq!(host2.count_kind(ActivityKind::Orphaned), 2);
        // hourly は無傷。
        assert!(ids(&store).iter().any(|id| id.starts_with("hourly@")));
    }

    /// 初出のトリガーは「変更」ではないので rescheduled として報告しない。
    #[test]
    fn new_trigger_is_not_reported_as_rescheduled() {
        let mut host = FakeHost::default();
        let specs = vec![spec("hourly", "@hourly")];
        let store = store_of(vec![]);

        reconcile_at_startup(&mut host, &specs, &store, base());

        assert!(host.activities().is_empty());
    }

    // ---- 実行できないトリガー ------------------------------------------------

    /// 構成エラー / ロード失敗のトリガーは展開されず、既に積まれた予定は
    /// `[unavailable]` で破棄される (`[orphaned]` と言い分ける)。
    #[test]
    fn unrunnable_trigger_is_not_expanded_and_drops_its_tasks() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let mut broken = spec("broken", "@hourly");
        broken.runnable = false;
        // 前回起動時に積まれた予定が残っている状況。
        let store = store_of(vec![adhoc("manual-broken-1", "broken", base())]);

        heartbeat(&mut host, &[broken], &store, &mut state, base(), grace());

        assert_eq!(host.count_kind(ActivityKind::Unavailable), 1);
        assert_eq!(host.count_kind(ActivityKind::Expanded), 0);
        assert!(ids(&store).is_empty());
    }

    /// トリガーを持たないタスク (Phase 3 の自然言語タスク、あるいは手で編集された
    /// `tasks.json`) は Phase 1 では実行経路が無い。孤児にはせず `[unsupported]` として
    /// 破棄する — 「トリガーが消えた」と「まだ実行できない」は原因の切り分けが違う。
    #[test]
    fn task_without_a_trigger_is_unsupported() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let store = store_of(vec![Task {
            id: "note-1".into(),
            origin: TaskOrigin::Adhoc,
            trigger_id: None,
            scheduled_at: base(),
            created_at: base(),
        }]);

        heartbeat(&mut host, &[], &store, &mut state, base(), grace());

        assert_eq!(host.count_kind(ActivityKind::Unsupported), 1);
        assert_eq!(host.activities()[0].0, "__task__");
        assert!(ids(&store).is_empty());
    }

    /// manifest から消えたトリガーを指すタスクが心拍まで生き残った場合は `[orphaned]`。
    #[test]
    fn task_for_unknown_trigger_is_orphaned() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let store = store_of(vec![adhoc("manual-ghost-1", "ghost", base())]);

        heartbeat(&mut host, &[], &store, &mut state, base(), grace());

        assert_eq!(host.count_kind(ActivityKind::Orphaned), 1);
        assert!(ids(&store).is_empty());
    }

    // ---- 手動実行 ------------------------------------------------------------

    /// 手動実行で積まれた ad-hoc タスクは次の心拍で実行され、展開済み境界を触らない。
    #[test]
    fn manual_run_executes_without_moving_the_boundary() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply("hourly", Ok(Some(tick_result("手動実行"))));
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        let boundary_before = boundary(&store, "hourly");
        let scheduled_before = ids(&store);

        // run_trigger_now 相当: 即 due な ad-hoc を積んで心拍を起こす。
        let manual_at = base() + MINUTE;
        lock_tasks(&store).insert(adhoc("manual-hourly-1", "hourly", manual_at));
        heartbeat(&mut host, &specs, &store, &mut state, manual_at, grace());

        assert!(host
            .effects
            .contains(&Effect::Notify("hourly trigger".into(), "手動実行".into())));
        // 境界も定期タスクも動いていない。
        assert_eq!(boundary(&store, "hourly"), boundary_before);
        assert_eq!(ids(&store), scheduled_before);
    }

    /// ad-hoc が猶予 (24h) を超えると `[expired]` で破棄される。
    #[test]
    fn adhoc_beyond_grace_expires() {
        let mut state = WorkerState::default();
        let mut host = FakeHost::default();
        let specs = vec![spec("hourly", "@hourly")];
        let store = store_of(vec![adhoc("manual-hourly-1", "hourly", base())]);

        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + DAY + HOUR,
            grace(),
        );

        assert_eq!(host.count_kind(ActivityKind::Expired), 1);
        assert!(!ids(&store).contains(&"manual-hourly-1".to_string()));
    }

    // ---- 副作用の順序 --------------------------------------------------------

    /// state読 → tick → notify → state保存 → 削除+save の順であること。
    #[test]
    fn side_effects_happen_in_the_documented_order() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply(
            "hourly",
            Ok(Some(TickResult {
                notify: Some(NotifyPayload {
                    title: Some("明示タイトル".into()),
                    body: "本文".into(),
                }),
                state: Some(serde_json::json!({"seen": 1})),
            })),
        );
        let store =
            store_without_expansion("hourly", vec![adhoc("manual-hourly-1", "hourly", base())]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        assert_eq!(
            host.effects,
            vec![
                // retention の掃除は実行より先。この心拍が積む履歴を消させないため (#42)。
                Effect::Sweep,
                Effect::ReadState("hourly".into()),
                Effect::Tick("hourly".into()),
                Effect::Notify("明示タイトル".into(), "本文".into()),
                Effect::Activity("hourly".into(), "notify", "本文".into()),
                Effect::WriteState("hourly".into(), r#"{"seen":1}"#.into()),
                Effect::Save(0),
            ]
        );
    }

    /// 実行中 (state 保存の直前) にクラッシュしてもタスクは残る = at-least-once。
    /// 次回起動でもう一度実行される。
    #[test]
    fn crash_before_cleanup_keeps_the_task() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply(
            "hourly",
            Ok(Some(TickResult {
                notify: Some(NotifyPayload {
                    title: None,
                    body: "通知は出た".into(),
                }),
                state: Some(serde_json::json!({"seen": 1})),
            })),
        );
        host.panic_on_write_state = Some("hourly".into());
        let store =
            store_without_expansion("hourly", vec![adhoc("manual-hourly-1", "hourly", base())]);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        }));
        assert!(result.is_err(), "write_state で panic するはず");

        // 通知は既に出ている (notify が state 保存より先)。
        assert!(host.effects.contains(&Effect::Notify(
            "hourly trigger".into(),
            "通知は出た".into()
        )));
        // タスクは消えていない (削除は後片付けの段階でしか起きない)。
        assert_eq!(ids(&store), vec!["manual-hourly-1".to_string()]);
        // 永続化も走っていない = tasks.json に残る = 次回起動でもう一度実行される。
        assert_eq!(host.saves, 0);
    }

    /// panic を跨いでもタスクリストは触れ続けられる。
    ///
    /// due 取り出しでロックを手放してから JS を回すので、[`heartbeat`] の中で panic しても
    /// 実際には poison しない。poison するのは UI コマンド側 (ロックを保持したまま
    /// `save_task_store` を呼ぶ経路) で panic した場合であり、そこで秘書が二度と動かなく
    /// なることを防ぐのが [`lock_tasks`] の役目。
    #[test]
    fn poisoned_lock_is_recovered() {
        let store = store_of(vec![adhoc("manual-hourly-1", "hourly", base())]);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock_tasks(&store);
            panic!("simulated crash while holding the task list");
        }));
        assert!(result.is_err());
        assert!(store.is_poisoned());

        // 掴み直せて、中身も失われていない。
        assert_eq!(ids(&store), vec!["manual-hourly-1".to_string()]);
    }

    /// tick() がエラーを返してもタスクは消す (毎心拍リトライのノイズを避ける)。
    #[test]
    fn tick_error_still_consumes_the_task() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply("hourly", Err("ReferenceError: x".into()));
        let store = store_of(vec![adhoc("manual-hourly-1", "hourly", base())]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        assert_eq!(host.count_kind(ActivityKind::Error), 1);
        assert!(!ids(&store).contains(&"manual-hourly-1".to_string()));
    }

    /// notify を返さない tick() は通知も activity も出さない (静かに終わる)。
    #[test]
    fn tick_without_notify_is_silent() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply("hourly", Ok(None));
        let store =
            store_without_expansion("hourly", vec![adhoc("manual-hourly-1", "hourly", base())]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        assert!(!host.effects.iter().any(|e| matches!(e, Effect::Notify(..))));
        assert!(host.activities().is_empty());
    }

    /// 同一心拍で複数トリガーが due になっても 1 回しか永続化しない。
    #[test]
    fn one_save_per_heartbeat() {
        let mut state = WorkerState::default();
        let specs = vec![spec("a", "@hourly"), spec("b", "@hourly")];
        let mut host = FakeHost::default();
        let store = store_of(vec![
            adhoc("manual-a-1", "a", base()),
            adhoc("manual-b-1", "b", base()),
        ]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        assert_eq!(host.saves, 1);
        assert!(host.saved.iter().all(|id| !id.starts_with("manual-")));
    }

    // ---- 長期停止からの復帰 (#50) --------------------------------------------

    /// 2 日閉じていた後の起動。0.2.0 では心拍が `[skipped]` を 1 件ずつ出して観測面を
    /// 埋め、起動時にしか出ない `[expanded]` を押し流していた。起動時に畳んで捨てる。
    #[test]
    fn long_downtime_collapses_into_one_line_per_trigger() {
        let specs = vec![spec("hourly", "@hourly"), spec("daily", "@daily 06:00")];
        let store = store_of(vec![]);

        // 2 日前に起動して 48h ぶんを積み、そのまま閉じた状況を作る。
        let before = base() - 2 * DAY;
        {
            let mut warmup = FakeHost::default();
            let mut state = WorkerState::default();
            heartbeat(&mut warmup, &specs, &store, &mut state, before, grace());
        }
        let stacked = ids(&store).len();
        assert!(stacked > 40, "前提: 相応の件数が積まれている ({stacked})");

        // 2 日後に起動。掃除は心拍が行う (#53)。
        let mut host = FakeHost::default();
        let mut state = WorkerState::default();
        reconcile_at_startup(&mut host, &specs, &store, base());
        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        // トリガーごとに 1 行だけ。
        assert_eq!(host.count_kind(ActivityKind::Stale), 2);
        assert_eq!(host.count_kind(ActivityKind::Skipped), 0);

        let hourly = host
            .activities
            .iter()
            .find(|a| a.kind == ActivityKind::Stale && a.source == "hourly")
            .unwrap();
        // 48 件積まれたうち、ちょうど base() に来る 1 件は猶予内なので残る (心拍が実行する)。
        // 境界のこちら側とあちら側で扱いが分かれることの確認でもある。
        assert_eq!(
            hourly.display(),
            format!(
                "[stale] 47 件の予定を遅延で破棄しました (最古 {})",
                fmt_utc(before + HOUR)
            )
        );
        // 48 件のうち base() ちょうどに来る 1 件は猶予内なので破棄されず、
        // 同じ心拍で実行される。境界のこちら側とあちら側で扱いが分かれることの確認。
        assert!(host.effects.contains(&Effect::Tick("hourly".into())));

        // 続く心拍は静か。破棄済みなので同じ行が再び出ることはない。
        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + MINUTE,
            grace(),
        );
        assert_eq!(host.count_kind(ActivityKind::Stale), 2);
        assert_eq!(host.count_kind(ActivityKind::Skipped), 0);
        // その回にしか出ないイベントが埋もれない。
        assert_eq!(host.count_kind(ActivityKind::Expanded), 2);
    }

    /// 掃除は心拍にもある (#53)。**プロセスを保ったままのスリープ復帰**は
    /// `reconcile` を通らないので、起動時だけの特別扱いでは塞がらなかった。
    #[test]
    fn sleep_resume_without_restart_is_also_collapsed() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default();
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        // 再起動しない = reconcile_at_startup を通らないまま 2 日進む。
        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + 2 * DAY,
            grace(),
        );

        // 0.2.0 / #50 時点ではここが 47 件の [skipped] だった。
        assert_eq!(host.count_kind(ActivityKind::Skipped), 0);
        assert_eq!(host.count_kind(ActivityKind::Stale), 1);
        assert_eq!(
            host.only(ActivityKind::Stale).display(),
            format!(
                "[stale] 47 件の予定を遅延で破棄しました (最古 {})",
                fmt_utc(base() + HOUR)
            )
        );
    }

    // ---- 履歴 (#42) ----------------------------------------------------------

    /// retention の掃除は起動直後に 1 回走り、その後は閾値 (1h) 条件で走る。
    /// 心拍ごとに DELETE を撃たないこと。
    #[test]
    fn history_sweep_follows_the_threshold() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default();
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        assert_eq!(host.sweeps, 1, "起動直後は必ず 1 回走る");

        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + 30 * MINUTE,
            grace(),
        );
        assert_eq!(host.sweeps, 1, "閾値未満では走らない");

        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + HOUR,
            grace(),
        );
        assert_eq!(host.sweeps, 2);
    }

    /// タスク由来のイベントは元のタスクを持つ。実行後にタスクは消えるので、履歴側は
    /// スナップショットとして持つしかない (#42)。
    #[test]
    fn task_derived_activities_carry_the_task() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default().reply("hourly", Ok(Some(tick_result("定時報告"))));
        let store =
            store_without_expansion("hourly", vec![adhoc("manual-hourly-1", "hourly", base())]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        let notify = host.only(ActivityKind::Notify);
        let task = notify.task.as_ref().expect("通知はタスク由来");
        assert_eq!(task.id, "manual-hourly-1");
        assert_eq!(task.origin, TaskOrigin::Adhoc);
        assert_eq!(task.scheduled_at, base());
    }

    /// 破棄が 1 件だけなら畳まず、元のタスクを持った `[skipped]` を出す。
    /// 「どの予定が流れたか」は 1 件のときこそ情報として意味を持つ (#53)。
    #[test]
    fn a_single_stale_task_keeps_its_identity() {
        let mut state = WorkerState::default();
        let specs = vec![spec("daily", "@daily 06:00")];
        let mut host = FakeHost::default();
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());
        // 06:00 の予定を 3 時間遅れで拾う。積まれているのは 1 件だけ。
        heartbeat(
            &mut host,
            &specs,
            &store,
            &mut state,
            base() + 9 * HOUR,
            grace(),
        );

        assert_eq!(host.count_kind(ActivityKind::Stale), 0);
        let skipped = host.only(ActivityKind::Skipped);
        assert_eq!(
            skipped.display(),
            format!(
                "[skipped] {} の予定に 3h0m 遅れているため実行しませんでした",
                fmt_utc(base() + 6 * HOUR)
            )
        );
        assert_eq!(
            skipped.task.as_ref().unwrap().scheduled_at,
            base() + 6 * HOUR
        );
    }

    /// 展開・再展開はタスクに紐付かない (トリガー単位のイベント)。
    #[test]
    fn trigger_level_activities_have_no_task() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default();
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        assert!(host.only(ActivityKind::Expanded).task.is_none());
    }

    /// 表示用の 1 行は 0.2.0 と同じ形。kind を列に出したことで UI の見た目が
    /// 変わってはいけない (#42 のスコープは永続化であって UI ではない)。
    #[test]
    fn display_strings_are_unchanged_from_0_2_0() {
        let mut state = WorkerState::default();
        let specs = vec![spec("hourly", "@hourly")];
        let mut host = FakeHost::default();
        let store = store_of(vec![]);

        heartbeat(&mut host, &specs, &store, &mut state, base(), grace());

        assert_eq!(
            host.only(ActivityKind::Expanded).display(),
            "[expanded] 48 件のタスクを積みました (〜2026-01-03T00:00:00Z)"
        );
    }
}
