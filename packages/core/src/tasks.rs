//! タスクリスト: スケジュールの唯一の実体 (#26 Phase 1)。
//!
//! `manifest.schedule` は「発火条件」ではなく**絶対時刻の生成規則**として扱う。展開器が
//! 具体的な時刻を持つタスクに変換してこのリストに積み、心拍は「due なタスクを取り出して
//! 実行して消す」だけを行う。`should_fire` の分岐も interval の経過時間計算も実行パスから
//! 消えている。
//!
//! ```text
//! manifest.schedule ──(展開器: 閾値条件)──▶ タスクリスト ──(心拍: due 取り出し)──▶ 実行
//!                                              ▲                                  │
//! 手動実行 / (将来) Type II ──(直接積む)────────┘                                  ▼
//!                                                                            実行/破棄の記録
//! ```
//!
//! # 設計の要点
//!
//! **展開済み境界 (決定事項 3)**: トリガーごとに `expanded_until` を 1 つだけ持つ。展開器は
//! この境界より後の時刻しか生成しない。境界より前にあった「秘書 AI やエンドユーザーが消した
//! タスク」は二度と生成されないので、tombstone 無しで冪等性が自明になる。
//!
//! **閾値条件で起こす (決定事項 6)**: Chamberlain はデスクトップアプリで常時起動していない。
//! 日次パスを 00:00 に置くと 9:00〜18:00 しか PC を開かないユーザーの環境では一度も走らない。
//! 代わりに心拍ごとに `expanded_until < now + THRESHOLD` を判定する。比較 1 つなのでコストは
//! 無視でき、「結果として 1 日 1 回程度走る」が保証される。
//!
//! **過去は生成しない**: 展開の起点は `max(expanded_until, now)` である。1 週間アプリを閉じて
//! いた後の起動で 1 週間分の過去タスクを生成してから全部破棄する、という無駄と観測面のノイズを
//! 避ける。wall-clock の missed-fire skip (#18) は「生成しない」ことで達成される。
//! 決定事項 8 の遅延分岐が効くのは「リストに積まれた後に時間が飛んだ」場合 (スリープ復帰・
//! 心拍の遅れ) だけになる。
//!
//! # 猶予期間が origin で違う理由
//!
//! Issue #26 決定事項 8 のフロー図は「遅れ < 猶予 → 実行」を両 origin 共通に書いているが、
//! 猶予を 24h 一律にすると「10:00 に起動して当日 09:00 の挨拶が実行される」ことになり、同じ
//! 決定事項が挙げている「今更 09:00 の挨拶をしても不自然」と矛盾する。両方の意図を満たす
//! 読みは 1 つしかないので、猶予を origin 別にしている:
//!
//! - **schedule 由来**: 心拍 2 回分。「心拍が拾い損ねた」ぶんだけを許し、それを超えた遅れは
//!   破棄する (#18 の missed-fire policy: skip と整合)。定期タスクには次の機会がある
//! - **ad-hoc**: [`ADHOC_GRACE`] (24h)。一回きりで消えたら二度と来ないので破棄してはならない。
//!   遅延を明示して実行する
//!
//! # 将来の拡張について
//!
//! [`Task`] に `prompt` 等のフィールドは今は持たせない ((a) トリガー実行のみ)。serde は
//! 未知フィールドを既定で無視するので、Phase 3 でフィールドを増やしても古い
//! `tasks.json` はそのまま読める。逆方向 (新 → 旧) は 0.x の範囲で保証しない。

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::schedule::{next_scheduled_after, Schedule};

/// 展開の先読み幅。閾値との差 (48h - 24h = 24h) が余裕として働く。
pub(crate) const EXPAND_HORIZON: Duration = Duration::from_secs(48 * 60 * 60);

/// 展開を起こす閾値。`expanded_until` がここまで迫ったら展開する (決定事項 6)。
pub(crate) const EXPAND_THRESHOLD: Duration = Duration::from_secs(24 * 60 * 60);

/// ad-hoc タスクの猶予期間。これより古い ad-hoc は「期限切れで未実行」として破棄する。
pub(crate) const ADHOC_GRACE: Duration = Duration::from_secs(24 * 60 * 60);

/// 1 回の展開で 1 トリガーが生成できるタスク数の上限。
/// 48h × `@every 5m` = 576 件が現実的な最大値なので、その 2 倍強を安全弁に置く。
/// 生成規則のバグや壊れた境界値で無限にメモリを食うことを防ぐのが目的。
const MAX_TASKS_PER_EXPANSION: usize = 1500;

/// タスクの出自。決定事項 8 の遅延分岐と、追加決定 9 の孤児掃除で分岐に使う。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TaskOrigin {
    /// `manifest.schedule` の展開器が生成した。過ぎたら破棄する。
    Schedule,
    /// 手動実行、または将来のチャット / Type II 由来。猶予内なら遅れても実行する。
    Adhoc,
}

/// 未来への意図 1 件。「今何が積まれているか」だけを表し、完了したものは即座に消える
/// (決定事項 1)。過去の記録は実行履歴レイヤ (Phase 2) の担当。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Task {
    /// schedule 由来は `"{trigger_id}@{scheduled_at}"` で決定的に決まる。展開済み境界だけでも
    /// 冪等性は担保されるが、境界の書き込みが失敗した場合の二重生成を id で弾けるようにしておく。
    pub id: String,
    pub origin: TaskOrigin,
    /// 実行対象のトリガー。Phase 1 では常に `Some`。Phase 3 で自然言語タスクが載ると
    /// `None` (トリガーを持たない ad-hoc) が現れる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<String>,
    /// 実行を意図された絶対時刻 (ms since epoch, UTC)。遅延は `now - scheduled_at` で
    /// 導出する。遅延そのものをフィールドに持たない (追加決定 11)。
    pub scheduled_at: u64,
    /// リストに積まれた時刻。UI で「いつ決まった予定か」を出すのに使う。
    pub created_at: u64,
}

/// トリガーごとの展開状態。持つのは境界 1 つ + 前回の schedule 文字列だけ。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct ExpansionState {
    /// ここまでは展開済み。展開器はこれより後の時刻しか生成しない。
    pub expanded_until: u64,
    /// 前回展開時の `manifest.schedule`。起動時にこれと比較して変更を検知する (決定事項 7)。
    pub schedule: String,
    /// 前回展開時の `manifest.tz`。schedule 文字列が同じでも tz が変われば時刻は変わる。
    #[serde(default)]
    pub tz: Option<String>,
}

/// `tasks.json` の中身。pending なタスクと、トリガーごとの展開状態。
#[derive(Serialize, Deserialize, Default, Debug)]
pub(crate) struct TaskStore {
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub expansion: BTreeMap<String, ExpansionState>,
}

impl TaskStore {
    /// `scheduled_at` 昇順に並べ直す。due 取り出しを先頭からの走査で済ませるための不変条件。
    /// 同時刻はタスク id で決定的に並べる (再起動をまたいで順序が揺れないようにする)。
    fn sort(&mut self) {
        self.tasks
            .sort_by(|a, b| a.scheduled_at.cmp(&b.scheduled_at).then(a.id.cmp(&b.id)));
    }

    /// 永続ファイルから読んだ直後に不変条件 (昇順・id 一意) を回復する。
    ///
    /// `tasks.json` はエンドユーザーが手で編集できる場所にあるので、順序も一意性も
    /// 信用しない。id 重複は先勝ちで落とす ([`TaskStore::insert`] と同じ方針)。
    pub(crate) fn normalize(&mut self) {
        self.sort();
        let mut seen = HashSet::new();
        self.tasks.retain(|t| seen.insert(t.id.clone()));
    }

    /// タスクを積む。同じ id が既にあれば何もしない (展開の二重実行に対する保険)。
    /// 戻り値は実際に積まれたか。
    pub(crate) fn insert(&mut self, task: Task) -> bool {
        if self.tasks.iter().any(|t| t.id == task.id) {
            return false;
        }
        self.tasks.push(task);
        self.sort();
        true
    }

    /// 複数タスクをまとめて積む。展開器は 1 回で数百件を返すので、id の重複判定を
    /// HashSet 1 本で済ませ、並べ替えも最後の 1 回だけにする。戻り値は実際に積まれた件数。
    pub(crate) fn extend(&mut self, incoming: impl IntoIterator<Item = Task>) -> usize {
        let mut ids: HashSet<String> = self.tasks.iter().map(|t| t.id.clone()).collect();
        let mut added = 0;
        for task in incoming {
            if ids.insert(task.id.clone()) {
                self.tasks.push(task);
                added += 1;
            }
        }
        if added > 0 {
            self.sort();
        }
        added
    }

    /// id でタスクを削除する。戻り値は実際に消えたか。
    pub(crate) fn remove(&mut self, id: &str) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.id != id);
        self.tasks.len() != before
    }

    /// `scheduled_at <= now` なタスクを昇順で返す (リストからは消さない)。
    ///
    /// 消さない理由: 実行 → 削除 の順にすることで、実行中のクラッシュ時にタスクが残り、
    /// 次回起動でもう一度実行される。既存の notify 先行ポリシーと同じ「1 回多く言う >
    /// 一言忘れる」の立場を取る。
    pub(crate) fn due(&self, now: u64) -> Vec<Task> {
        self.tasks
            .iter()
            .take_while(|t| t.scheduled_at <= now)
            .cloned()
            .collect()
    }

    /// 指定トリガーの pending タスクのうち最も早い `scheduled_at`。UI の「次回発火予定」用。
    pub(crate) fn next_scheduled_for(&self, trigger_id: &str) -> Option<u64> {
        self.tasks
            .iter()
            .filter(|t| t.trigger_id.as_deref() == Some(trigger_id))
            .map(|t| t.scheduled_at)
            .min()
    }

    /// 永続層から読んだ生の JSON 値を [`TaskStore`] に組み立てる。
    ///
    /// **壊れた値は「空」として扱い、起動を止めない。** タスクリストは再展開で復元できる
    /// (境界が失われても 1 回だけ余分に展開されるだけで、冪等性の観点でも安全側に倒れる)。
    /// `tasks.json` はエンドユーザーが手で編集できる場所にあるので、これは想定内の入力である。
    ///
    /// 戻り値の 2 つめは呼び出し側が観測面/stderr に流すための警告。ここで直接 eprintln
    /// しないのは、この関数を副作用なしにしてテストできるようにするため (#46)。
    pub(crate) fn from_stored(
        tasks: Option<serde_json::Value>,
        expansion: Option<serde_json::Value>,
    ) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();

        let tasks = tasks
            .and_then(|v| match serde_json::from_value::<Vec<Task>>(v) {
                Ok(t) => Some(t),
                Err(e) => {
                    warnings.push(format!(
                        "tasks.json: unreadable task list, starting empty: {e}"
                    ));
                    None
                }
            })
            .unwrap_or_default();

        let expansion = expansion
            .and_then(
                |v| match serde_json::from_value::<BTreeMap<String, ExpansionState>>(v) {
                    Ok(e) => Some(e),
                    Err(e) => {
                        warnings.push(format!(
                            "tasks.json: unreadable expansion state, starting empty: {e}"
                        ));
                        None
                    }
                },
            )
            .unwrap_or_default();

        let mut loaded = TaskStore { tasks, expansion };
        // due 取り出しは「先頭から scheduled_at 昇順」を前提にしている。手で編集された
        // tasks.json を読んだ場合にもこの不変条件を成立させる。
        loaded.normalize();
        (loaded, warnings)
    }

    /// 永続層に書く 2 つの JSON 値 (tasks / expansion) を作る。
    pub(crate) fn to_stored(&self) -> Result<(serde_json::Value, serde_json::Value), String> {
        let tasks = serde_json::to_value(&self.tasks)
            .map_err(|e| format!("failed to serialize task list: {e}"))?;
        let expansion = serde_json::to_value(&self.expansion)
            .map_err(|e| format!("failed to serialize expansion state: {e}"))?;
        Ok((tasks, expansion))
    }
}

/// due になったタスクの処理方針。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// 実行する。`delay_ms` は意図時刻からの遅れ (通常 0 に近い)。
    Run { delay_ms: u64 },
    /// 親トリガーが存在しない (アプリ更新で manifest から消えた)。破棄 (追加決定 9)。
    Orphaned,
    /// 親トリガーが pause 中。破棄 (追加決定 10)。
    Paused,
    /// schedule 由来で猶予を超えて遅れた。破棄 (#18 missed-fire policy: skip)。
    SkippedLate { delay_ms: u64 },
    /// ad-hoc で猶予を超えて遅れた。破棄するが痕跡は残す。
    Expired { delay_ms: u64 },
}

/// 親トリガーの状態。`None` は「そのトリガーが現存しない」を意味する。
pub(crate) type TriggerState = Option<TriggerRuntimeState>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TriggerRuntimeState {
    pub paused: bool,
}

/// due なタスク 1 件をどう扱うか決める。副作用を持たないので単体でテストできる。
///
/// 判定順は「なぜ実行しないか」の情報量が多い順: 孤児 → pause → 遅延。pause 中で
/// かつ遅れているタスクは `Paused` と報告する (エージェント開発者にとって「止めていたから」の
/// 方が「遅すぎたから」より有用な情報)。
pub(crate) fn classify_due(
    task: &Task,
    now: u64,
    trigger: TriggerState,
    schedule_grace: Duration,
) -> Disposition {
    // trigger_id を持たないタスク (Phase 3 の自然言語タスク) は親を参照しないので、
    // 孤児判定・pause 判定の対象外。Phase 1 では発生しない。
    if task.trigger_id.is_some() {
        match trigger {
            None => return Disposition::Orphaned,
            Some(state) if state.paused => return Disposition::Paused,
            Some(_) => {}
        }
    }

    let delay_ms = now.saturating_sub(task.scheduled_at);
    let grace_ms = match task.origin {
        TaskOrigin::Schedule => schedule_grace.as_millis() as u64,
        TaskOrigin::Adhoc => ADHOC_GRACE.as_millis() as u64,
    };
    if delay_ms <= grace_ms {
        return Disposition::Run { delay_ms };
    }
    match task.origin {
        TaskOrigin::Schedule => Disposition::SkippedLate { delay_ms },
        TaskOrigin::Adhoc => Disposition::Expired { delay_ms },
    }
}

/// トリガー 1 つ分を展開する。境界より後・ホライズン以内の時刻タスクを生成し、
/// 新しい境界を返す。
///
/// 起点は `max(expanded_until, now)`。過去タスクを生成しないことで missed-fire skip が
/// 「生成しない」で達成される (モジュール doc 参照)。
pub(crate) fn expand_trigger(
    trigger_id: &str,
    spec: &Schedule,
    tz: &Tz,
    expanded_until: u64,
    now: u64,
) -> (Vec<Task>, u64) {
    let horizon_end = now.saturating_add(EXPAND_HORIZON.as_millis() as u64);
    let mut cursor = expanded_until.max(now);
    let mut generated = Vec::new();

    while generated.len() < MAX_TASKS_PER_EXPANSION {
        let Some(next) = next_scheduled_after(cursor, spec, tz) else {
            // 以降 fire しない (`@at` の期日を過ぎた等)。境界だけ進めて打ち切る。
            break;
        };
        if next > horizon_end {
            break;
        }
        generated.push(Task {
            id: format!("{trigger_id}@{next}"),
            origin: TaskOrigin::Schedule,
            trigger_id: Some(trigger_id.to_string()),
            scheduled_at: next,
            created_at: now,
        });
        cursor = next;
    }

    (generated, horizon_end)
}

/// 展開が必要か (決定事項 6 の閾値条件)。心拍ごとに呼ばれるので比較 1 つで済ませる。
pub(crate) fn needs_expansion(expanded_until: u64, now: u64) -> bool {
    expanded_until < now.saturating_add(EXPAND_THRESHOLD.as_millis() as u64)
}

/// 起動時の突き合わせに渡すトリガーの見え方。
pub(crate) struct TriggerSpecView<'a> {
    pub id: &'a str,
    /// manifest の生の schedule 文字列 (パース結果ではなく文字列を比較する)。
    pub schedule: &'a str,
    pub tz: Option<&'a str>,
}

/// 起動時突き合わせの結果。呼び出し側が観測面に流すために使う。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcileReport {
    /// 親トリガーが消えたので破棄したタスク (task id, trigger id)。
    pub orphaned: Vec<(String, String)>,
    /// schedule / tz が変わったので未実行タスクを捨てて再展開したトリガー id。
    pub rescheduled: Vec<String>,
    /// 猶予を超えて古くなっていたので一括破棄した予定 (トリガーごとに 1 件)。
    pub stale: Vec<StaleSweep>,
}

/// 長期停止からの復帰で一括破棄した予定の要約 (#50)。
///
/// 1 件ずつではなくトリガー単位に畳む。48h 先まで展開されたタスクが 2 日の停止で
/// 全部過去になると、`@every 5m` なら 576 件になる。同じことを言う行が観測面を埋め、
/// **起動時にしか出ない `[expanded]` / `[rescheduled]` を押し流してしまう**。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StaleSweep {
    pub trigger_id: String,
    pub count: usize,
    /// 破棄したうち最も古い `scheduled_at`。「いつから止まっていたか」が読める。
    pub oldest: u64,
}

/// 起動時に永続タスクリストを現在の manifest と突き合わせる。
///
/// 1. **孤児掃除 (追加決定 9)** — 現存しないトリガーを指すタスクと、その展開状態を破棄する。
///    放置すると心拍が `trigger_id` を解決できず、due になった瞬間から失敗し続ける
/// 2. **schedule 変更検知 (決定事項 7)** — `schedule` / `tz` 文字列が前回と違うトリガーは、
///    未実行の schedule 由来タスクを捨てて境界を `now` に戻す。次の展開パスで新しい
///    スケジュールのタスクが積まれる
///
/// 猶予窓は設けない。Mastra の `missesBeforeDelete` は「デプロイ順序でスケジューラが
/// ワークフロー登録より先に回る」レース対策だが、Chamberlain の `discover_triggers()` は
/// 起動時 1 回で確定するのでこのレースが存在しない。
pub(crate) fn reconcile(
    store: &mut TaskStore,
    triggers: &[TriggerSpecView<'_>],
    now: u64,
    schedule_grace: Duration,
) -> ReconcileReport {
    let known: HashSet<&str> = triggers.iter().map(|t| t.id).collect();
    let mut report = ReconcileReport::default();

    // 1. 孤児掃除。origin ではなく trigger_id の有無で判定する。手動実行で積まれた
    //    ad-hoc タスクも、指しているトリガーが消えていれば同じく実行不能なため。
    store.tasks.retain(|t| match t.trigger_id.as_deref() {
        Some(tid) if !known.contains(tid) => {
            report.orphaned.push((t.id.clone(), tid.to_string()));
            false
        }
        _ => true,
    });
    store
        .expansion
        .retain(|tid, _| known.contains(tid.as_str()));

    // 2. schedule 変更検知。
    //
    // 前回値は所有した形で先に取り出す。`get_mut` した参照を保持したまま同じ map に
    // `insert` する形は借用が重なって通らないため、判定 → 変更の順に分ける。
    // 起動時 1 回・トリガー数個の経路なので短い String 2 本の clone は無視できる。
    for view in triggers {
        let previous = store
            .expansion
            .get(view.id)
            .map(|s| (s.schedule.clone(), s.tz.clone()));

        let unchanged = matches!(
            &previous,
            Some((schedule, tz)) if schedule == view.schedule && tz.as_deref() == view.tz
        );
        if unchanged {
            continue;
        }

        let is_new = previous.is_none();
        store.expansion.insert(
            view.id.to_string(),
            ExpansionState {
                expanded_until: now,
                schedule: view.schedule.to_string(),
                tz: view.tz.map(str::to_string),
            },
        );

        // 初出のトリガーは「変更」ではないので報告もタスク破棄もしない。
        if is_new {
            continue;
        }
        report.rescheduled.push(view.id.to_string());
        // schedule 由来だけを捨てる。手動実行で積まれた ad-hoc タスクは
        // schedule 文字列とは無関係の意図なので残す。
        let tid = view.id;
        store.tasks.retain(|t| {
            !(t.origin == TaskOrigin::Schedule && t.trigger_id.as_deref() == Some(tid))
        });
    }

    // 3. 長期停止からの復帰の掃除 (#50)。
    //
    // 猶予を超えた schedule 由来タスクは **絶対に実行されない** ([`classify_due`] が
    // `SkippedLate` を返す)。それを心拍で 1 件ずつ発見すると、2 日の停止で数十〜数百件の
    // 同じ行が観測面を埋める。判定は心拍と同一で、報告だけをトリガー単位に畳む。
    //
    // pause 状態を見ないのは、**pause が永続化されていない**ため。起動直後は必ず
    // 全トリガーが実行中で、`Disposition::Paused` に化ける行は存在しない。
    let cutoff = now.saturating_sub(schedule_grace.as_millis() as u64);
    let mut swept: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    store.tasks.retain(|t| {
        if t.origin != TaskOrigin::Schedule || t.scheduled_at >= cutoff {
            return true;
        }
        let key = t
            .trigger_id
            .clone()
            .unwrap_or_else(|| "__task__".to_string());
        let entry = swept.entry(key).or_insert((0, t.scheduled_at));
        entry.0 += 1;
        entry.1 = entry.1.min(t.scheduled_at);
        false
    });
    report.stale = swept
        .into_iter()
        .map(|(trigger_id, (count, oldest))| StaleSweep {
            trigger_id,
            count,
            oldest,
        })
        .collect();

    store.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::parse_schedule;

    const HOUR: u64 = 60 * 60 * 1000;
    const DAY: u64 = 24 * HOUR;

    /// 心拍 1 分 × 猶予 2 回分 (prod 相当)。
    fn grace() -> Duration {
        Duration::from_secs(120)
    }

    fn tz_utc() -> Tz {
        "UTC".parse().unwrap()
    }

    /// 2026-01-01T00:00:00Z
    fn base() -> u64 {
        1_767_225_600_000
    }

    fn schedule_task(trigger: &str, at: u64) -> Task {
        Task {
            id: format!("{trigger}@{at}"),
            origin: TaskOrigin::Schedule,
            trigger_id: Some(trigger.to_string()),
            scheduled_at: at,
            created_at: at,
        }
    }

    fn adhoc_task(trigger: Option<&str>, at: u64) -> Task {
        Task {
            id: format!("adhoc-{at}"),
            origin: TaskOrigin::Adhoc,
            trigger_id: trigger.map(str::to_string),
            scheduled_at: at,
            created_at: at,
        }
    }

    fn running() -> TriggerState {
        Some(TriggerRuntimeState { paused: false })
    }

    fn paused() -> TriggerState {
        Some(TriggerRuntimeState { paused: true })
    }

    // ---- 展開器 ----

    #[test]
    fn expand_generates_tasks_within_horizon() {
        let spec = parse_schedule("@daily 09:00").unwrap();
        let now = base(); // 00:00Z
        let (tasks, boundary) = expand_trigger("greeter", &spec, &tz_utc(), now, now);
        // ホライズン 48h 内の 09:00 は 2 回 (当日と翌日)
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].scheduled_at, now + 9 * HOUR);
        assert_eq!(tasks[1].scheduled_at, now + DAY + 9 * HOUR);
        assert_eq!(boundary, now + 2 * DAY);
        assert!(tasks.iter().all(|t| t.origin == TaskOrigin::Schedule));
        assert_eq!(tasks[0].trigger_id.as_deref(), Some("greeter"));
    }

    #[test]
    fn expand_every_5m_stays_under_cap() {
        let spec = parse_schedule("@every 5m").unwrap();
        let now = base();
        let (tasks, _) = expand_trigger("dense", &spec, &tz_utc(), now, now);
        // 48h × 12/h = 576 件。安全弁 (1500) には触れない。
        assert_eq!(tasks.len(), 576);
        assert!(tasks.len() < MAX_TASKS_PER_EXPANSION);
    }

    // 境界より後しか生成しない = 決定事項 3 の冪等性。同じ now で 2 回展開しても
    // 2 回目は 1 件も生まれない。
    #[test]
    fn expand_is_idempotent_via_boundary() {
        let spec = parse_schedule("@hourly").unwrap();
        let now = base();
        let (first, boundary) = expand_trigger("t", &spec, &tz_utc(), now, now);
        assert!(!first.is_empty());
        let (second, boundary2) = expand_trigger("t", &spec, &tz_utc(), boundary, now);
        assert!(
            second.is_empty(),
            "境界以降・ホライズン以内には残りが無いはず: {second:?}"
        );
        assert_eq!(boundary2, boundary);
    }

    // 消したタスクが次の展開で復活しないこと。境界が進んでいる限り、境界より前の
    // 時刻は二度と生成されない。
    #[test]
    fn expand_never_resurrects_deleted_task() {
        let spec = parse_schedule("@hourly").unwrap();
        let now = base();
        let mut store = TaskStore::default();
        let (tasks, boundary) = expand_trigger("t", &spec, &tz_utc(), now, now);
        for t in tasks {
            store.insert(t);
        }
        let victim = store.tasks[3].id.clone();
        let victim_at = store.tasks[3].scheduled_at;
        assert!(store.remove(&victim));

        // 少し時間が進んでから再展開
        let later = now + HOUR;
        let (again, _) = expand_trigger("t", &spec, &tz_utc(), boundary, later);
        assert!(
            !again.iter().any(|t| t.scheduled_at == victim_at),
            "消した {victim} が復活している"
        );
    }

    // 1 週間閉じていた後の起動で、過去 1 週間分のタスクを生成してはいけない。
    // 起点が max(expanded_until, now) であることの回帰テスト。
    #[test]
    fn expand_does_not_generate_past_tasks() {
        let spec = parse_schedule("@hourly").unwrap();
        let stale_boundary = base();
        let now = base() + 7 * DAY;
        let (tasks, boundary) = expand_trigger("t", &spec, &tz_utc(), stale_boundary, now);
        assert!(
            tasks.iter().all(|t| t.scheduled_at > now),
            "過去タスクが生成された: {:?}",
            tasks.iter().filter(|t| t.scheduled_at <= now).count()
        );
        assert_eq!(boundary, now + 2 * DAY);
    }

    // @at は 1 回展開されたら以降は何も生まない (next_scheduled_after が None を返す)。
    #[test]
    fn expand_at_yields_single_task_then_nothing() {
        let target = base() + 10 * HOUR;
        let dt = chrono::DateTime::from_timestamp_millis(target as i64)
            .unwrap()
            .naive_utc();
        let spec = Schedule::At { datetime: dt };
        let now = base();
        let (tasks, boundary) = expand_trigger("once", &spec, &tz_utc(), now, now);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].scheduled_at, target);
        let (again, _) = expand_trigger("once", &spec, &tz_utc(), boundary, now);
        assert!(again.is_empty());
    }

    #[test]
    fn needs_expansion_threshold() {
        let now = base();
        // 境界が 48h 先 → 閾値 24h より遠いので展開不要
        assert!(!needs_expansion(now + 2 * DAY, now));
        // 境界が 12h 先 → 閾値内なので展開する
        assert!(needs_expansion(now + 12 * HOUR, now));
        // 境界が過去 (長期間閉じていた) → 当然展開する
        assert!(needs_expansion(now - DAY, now));
    }

    // ---- due 取り出しと分類 ----

    #[test]
    fn due_returns_only_past_tasks_in_order() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("t", now + HOUR));
        store.insert(schedule_task("t", now - 2 * HOUR));
        store.insert(schedule_task("t", now - HOUR));
        let due = store.due(now);
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].scheduled_at, now - 2 * HOUR);
        assert_eq!(due[1].scheduled_at, now - HOUR);
        // due() はリストを消さない (実行 → 削除の順にするため)
        assert_eq!(store.tasks.len(), 3);
    }

    #[test]
    fn classify_runs_on_time_task() {
        let now = base();
        let task = schedule_task("t", now);
        assert_eq!(
            classify_due(&task, now, running(), Duration::from_secs(120)),
            Disposition::Run { delay_ms: 0 }
        );
    }

    // 心拍が 1 回遅れた程度は実行する。
    #[test]
    fn classify_runs_slightly_late_schedule_task() {
        let now = base();
        let task = schedule_task("t", now - 90_000); // 90s 遅れ
        assert_eq!(
            classify_due(&task, now, running(), Duration::from_secs(120)),
            Disposition::Run { delay_ms: 90_000 }
        );
    }

    // 決定事項 8 の核心: 「10:00 に起動して当日 09:00 の挨拶」は実行してはいけない。
    // 猶予が origin 別でなければこのテストは落ちる。
    #[test]
    fn classify_discards_stale_schedule_task() {
        let now = base();
        let task = schedule_task("greeter-morning", now - HOUR);
        assert_eq!(
            classify_due(&task, now, running(), Duration::from_secs(120)),
            Disposition::SkippedLate { delay_ms: HOUR }
        );
    }

    // ad-hoc は同じ 1 時間の遅れでも実行される。「明日 12:00 にリマインドして」が
    // 黙って消えるのは秘書として破綻しているため。
    #[test]
    fn classify_runs_late_adhoc_task_within_grace() {
        let now = base();
        let task = adhoc_task(Some("t"), now - HOUR);
        assert_eq!(
            classify_due(&task, now, running(), Duration::from_secs(120)),
            Disposition::Run { delay_ms: HOUR }
        );
    }

    // 猶予 (24h) を超えた ad-hoc は破棄する。「1 週間前のリマインドが起動時に一斉に鳴る」
    // のを防ぐ。
    #[test]
    fn classify_expires_ancient_adhoc_task() {
        let now = base();
        let task = adhoc_task(Some("t"), now - 7 * DAY);
        assert_eq!(
            classify_due(&task, now, running(), Duration::from_secs(120)),
            Disposition::Expired { delay_ms: 7 * DAY }
        );
    }

    #[test]
    fn classify_discards_paused_trigger_task() {
        let now = base();
        let task = schedule_task("t", now);
        assert_eq!(
            classify_due(&task, now, paused(), Duration::from_secs(120)),
            Disposition::Paused
        );
    }

    // pause が遅延より優先されること (情報量の多い理由を返す)。
    #[test]
    fn classify_reports_paused_over_late() {
        let now = base();
        let task = schedule_task("t", now - 7 * DAY);
        assert_eq!(
            classify_due(&task, now, paused(), Duration::from_secs(120)),
            Disposition::Paused
        );
    }

    #[test]
    fn classify_discards_orphaned_task() {
        let now = base();
        let task = schedule_task("gone", now);
        assert_eq!(
            classify_due(&task, now, None, Duration::from_secs(120)),
            Disposition::Orphaned
        );
    }

    // trigger_id を持たないタスク (Phase 3) は孤児判定の対象外。
    #[test]
    fn classify_ignores_trigger_state_for_triggerless_task() {
        let now = base();
        let task = adhoc_task(None, now);
        assert_eq!(
            classify_due(&task, now, None, Duration::from_secs(120)),
            Disposition::Run { delay_ms: 0 }
        );
    }

    // ---- 起動時突き合わせ ----

    // 追加決定 9: manifest から消えたトリガーのタスクは破棄される。
    #[test]
    fn reconcile_sweeps_orphaned_tasks() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("alive", now + HOUR));
        store.insert(schedule_task("gone", now + HOUR));
        store.expansion.insert(
            "gone".into(),
            ExpansionState {
                expanded_until: now + 2 * DAY,
                schedule: "@hourly".into(),
                tz: None,
            },
        );

        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "alive",
                schedule: "@hourly",
                tz: None,
            }],
            now,
            grace(),
        );

        assert_eq!(store.tasks.len(), 1);
        assert_eq!(store.tasks[0].trigger_id.as_deref(), Some("alive"));
        assert_eq!(report.orphaned.len(), 1);
        assert_eq!(report.orphaned[0].1, "gone");
        // 消えたトリガーの展開状態も残さない
        assert!(!store.expansion.contains_key("gone"));
    }

    // 手動実行で積まれた ad-hoc タスクも、指すトリガーが消えていれば破棄する。
    #[test]
    fn reconcile_sweeps_orphaned_adhoc_task() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(adhoc_task(Some("gone"), now));
        let report = reconcile(&mut store, &[], now, grace());
        assert!(store.tasks.is_empty());
        assert_eq!(report.orphaned.len(), 1);
    }

    // 決定事項 7: schedule 文字列が変わったら未実行タスクを捨てて境界を戻す。
    // これが無いと最大 48 時間、古いスケジュールで動き続ける。
    #[test]
    fn reconcile_reexpands_on_schedule_change() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("greeter", now + 9 * HOUR));
        store.insert(schedule_task("greeter", now + DAY + 9 * HOUR));
        store.expansion.insert(
            "greeter".into(),
            ExpansionState {
                expanded_until: now + 2 * DAY,
                schedule: "@daily 09:00".into(),
                tz: None,
            },
        );

        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "greeter",
                schedule: "@daily 08:00",
                tz: None,
            }],
            now,
            grace(),
        );

        assert_eq!(report.rescheduled, vec!["greeter".to_string()]);
        assert!(
            store.tasks.is_empty(),
            "古いスケジュールのタスクが残っている"
        );
        let state = &store.expansion["greeter"];
        assert_eq!(state.expanded_until, now);
        assert_eq!(state.schedule, "@daily 08:00");
    }

    // schedule 文字列が同じでも tz が変われば発火時刻が変わるので再展開する。
    #[test]
    fn reconcile_reexpands_on_tz_change() {
        let now = base();
        let mut store = TaskStore::default();
        store.expansion.insert(
            "greeter".into(),
            ExpansionState {
                expanded_until: now + 2 * DAY,
                schedule: "@daily 09:00".into(),
                tz: Some("Asia/Tokyo".into()),
            },
        );
        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "greeter",
                schedule: "@daily 09:00",
                tz: Some("Europe/Berlin"),
            }],
            now,
            grace(),
        );
        assert_eq!(report.rescheduled, vec!["greeter".to_string()]);
        assert_eq!(store.expansion["greeter"].expanded_until, now);
    }

    // 変更が無ければ境界もタスクも触らない (毎起動で再展開してはいけない)。
    #[test]
    fn reconcile_is_noop_when_unchanged() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("greeter", now + 9 * HOUR));
        store.expansion.insert(
            "greeter".into(),
            ExpansionState {
                expanded_until: now + 2 * DAY,
                schedule: "@daily 09:00".into(),
                tz: Some("Asia/Tokyo".into()),
            },
        );
        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "greeter",
                schedule: "@daily 09:00",
                tz: Some("Asia/Tokyo"),
            }],
            now,
            grace(),
        );
        assert_eq!(report, ReconcileReport::default());
        assert_eq!(store.tasks.len(), 1);
        assert_eq!(store.expansion["greeter"].expanded_until, now + 2 * DAY);
    }

    // schedule 変更で捨てるのは schedule 由来のみ。手動実行の意図は残す。
    #[test]
    fn reconcile_keeps_adhoc_tasks_on_schedule_change() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("greeter", now + 9 * HOUR));
        store.insert(adhoc_task(Some("greeter"), now + 60_000));
        store.expansion.insert(
            "greeter".into(),
            ExpansionState {
                expanded_until: now + 2 * DAY,
                schedule: "@daily 09:00".into(),
                tz: None,
            },
        );
        reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "greeter",
                schedule: "@daily 08:00",
                tz: None,
            }],
            now,
            grace(),
        );
        assert_eq!(store.tasks.len(), 1);
        assert_eq!(store.tasks[0].origin, TaskOrigin::Adhoc);
    }

    // 初出のトリガーは「変更」ではないので報告しない。境界だけ now に置かれる。
    #[test]
    fn reconcile_registers_new_trigger_without_reporting() {
        let now = base();
        let mut store = TaskStore::default();
        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "fresh",
                schedule: "@hourly",
                tz: None,
            }],
            now,
            grace(),
        );
        assert_eq!(report, ReconcileReport::default());
        assert_eq!(store.expansion["fresh"].expanded_until, now);
    }

    // ---- store の基本操作 ----

    #[test]
    fn insert_dedups_by_id() {
        let now = base();
        let mut store = TaskStore::default();
        assert!(store.insert(schedule_task("t", now)));
        assert!(!store.insert(schedule_task("t", now)));
        assert_eq!(store.tasks.len(), 1);
    }

    #[test]
    fn next_scheduled_for_picks_earliest() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("a", now + 2 * HOUR));
        store.insert(schedule_task("a", now + HOUR));
        store.insert(schedule_task("b", now + 30 * 60 * 1000));
        assert_eq!(store.next_scheduled_for("a"), Some(now + HOUR));
        assert_eq!(store.next_scheduled_for("b"), Some(now + 30 * 60 * 1000));
        assert_eq!(store.next_scheduled_for("missing"), None);
    }

    // 永続化のラウンドトリップ。未知フィールドを無視することも確認する
    // (Phase 3 でフィールドが増えても古い読み手が壊れないため)。
    #[test]
    fn store_json_roundtrip_tolerates_unknown_fields() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("t", now));
        store.expansion.insert(
            "t".into(),
            ExpansionState {
                expanded_until: now + DAY,
                schedule: "@hourly".into(),
                tz: None,
            },
        );
        let json = serde_json::to_value(&store).unwrap();
        let back: TaskStore = serde_json::from_value(json).unwrap();
        assert_eq!(back.tasks.len(), 1);
        assert_eq!(back.expansion["t"].expanded_until, now + DAY);

        let with_future_field = serde_json::json!({
            "tasks": [{
                "id": "x@1",
                "origin": "adhoc",
                "scheduled_at": 1,
                "created_at": 1,
                "prompt": "来週の月曜に催促して",
            }],
            "expansion": {},
        });
        let parsed: TaskStore = serde_json::from_value(with_future_field).unwrap();
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].origin, TaskOrigin::Adhoc);
        assert!(parsed.tasks[0].trigger_id.is_none());
    }

    // ---- 長期停止からの復帰 (#50) --------------------------------------------

    /// 2 日閉じていた後の起動を模す。積まれていた schedule 由来タスクが全部過去に
    /// なっているので、心拍で 1 件ずつ破棄せず起動時に畳んで捨てる。
    #[test]
    fn reconcile_sweeps_stale_schedule_tasks() {
        let now = base();
        let mut store = TaskStore::default();
        // 2 日前から 1 時間前まで、48 件すべてが過去。
        for i in 0..48u64 {
            store.insert(schedule_task("hourly", now - 2 * DAY + i * HOUR));
        }
        // 猶予内 (1 分遅れ) のものは残す。心拍が拾って実行する。
        store.insert(schedule_task("hourly", now - 60_000));
        store.expansion.insert(
            "hourly".into(),
            ExpansionState {
                expanded_until: now,
                schedule: "@hourly".into(),
                tz: None,
            },
        );

        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "hourly",
                schedule: "@hourly",
                tz: None,
            }],
            now,
            grace(),
        );

        assert_eq!(report.stale.len(), 1, "報告はトリガー単位に畳む");
        assert_eq!(report.stale[0].trigger_id, "hourly");
        assert_eq!(report.stale[0].count, 48);
        assert_eq!(report.stale[0].oldest, now - 2 * DAY);
        // 猶予内の 1 件だけ残る。
        assert_eq!(store.tasks.len(), 1);
        assert_eq!(store.tasks[0].scheduled_at, now - 60_000);
    }

    /// 判定は心拍の [`classify_due`] と同一でなければならない。境界のどちら側でも
    /// 同じ結論になることを直接突き合わせる。
    #[test]
    fn stale_cutoff_matches_the_heartbeat() {
        let now = base();
        let grace_ms = grace().as_millis() as u64;
        let alive = now - grace_ms; // ちょうど猶予 = 実行される
        let dead = now - grace_ms - 1; // 1ms 超過 = 破棄される

        for (at, should_survive) in [(alive, true), (dead, false)] {
            let task = schedule_task("t", at);
            let by_heartbeat = matches!(
                classify_due(
                    &task,
                    now,
                    Some(TriggerRuntimeState { paused: false }),
                    grace()
                ),
                Disposition::Run { .. }
            );
            assert_eq!(by_heartbeat, should_survive, "classify_due の判定");

            let mut store = TaskStore::default();
            store.insert(task);
            reconcile(
                &mut store,
                &[TriggerSpecView {
                    id: "t",
                    schedule: "@hourly",
                    tz: None,
                }],
                now,
                grace(),
            );
            assert_eq!(!store.tasks.is_empty(), should_survive, "reconcile の判定");
        }
    }

    /// ad-hoc は触らない。猶予 24h 内なら遅れても実行しなければならず (決定事項 8)、
    /// 超過分は心拍が `[expired]` として 1 件ずつ扱う (手動由来なので件数が知れている)。
    #[test]
    fn reconcile_leaves_adhoc_tasks_alone() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(adhoc_task(Some("t"), now - 2 * DAY));
        store.insert(adhoc_task(Some("t"), now - HOUR));

        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "t",
                schedule: "@hourly",
                tz: None,
            }],
            now,
            grace(),
        );

        assert!(report.stale.is_empty());
        assert_eq!(store.tasks.len(), 2);
    }

    /// トリガーが複数あれば報告も複数。1 行にまとめてしまうと「どのトリガーが
    /// 止まっていたか」が消える。
    #[test]
    fn stale_sweep_is_reported_per_trigger() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("a", now - DAY));
        store.insert(schedule_task("a", now - 2 * DAY));
        store.insert(schedule_task("b", now - 3 * HOUR));

        let views = [
            TriggerSpecView {
                id: "a",
                schedule: "@hourly",
                tz: None,
            },
            TriggerSpecView {
                id: "b",
                schedule: "@hourly",
                tz: None,
            },
        ];
        let report = reconcile(&mut store, &views, now, grace());

        assert_eq!(
            report
                .stale
                .iter()
                .map(|s| (s.trigger_id.as_str(), s.count, s.oldest))
                .collect::<Vec<_>>(),
            vec![("a", 2, now - 2 * DAY), ("b", 1, now - 3 * HOUR)]
        );
    }

    /// 未来のタスクは当然残る。長期停止からの復帰でなければ何も起きない。
    #[test]
    fn reconcile_is_a_noop_without_stale_tasks() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("t", now + HOUR));
        store.expansion.insert(
            "t".into(),
            ExpansionState {
                expanded_until: now + 2 * DAY,
                schedule: "@hourly".into(),
                tz: None,
            },
        );

        let report = reconcile(
            &mut store,
            &[TriggerSpecView {
                id: "t",
                schedule: "@hourly",
                tz: None,
            }],
            now,
            grace(),
        );

        assert_eq!(report, ReconcileReport::default());
        assert_eq!(store.tasks.len(), 1);
    }

    // ---- 永続層との境界 (#46) ------------------------------------------------

    /// 未設定の `tasks.json` (初回起動) は空の store になり、警告も出ない。
    #[test]
    fn from_stored_handles_a_missing_file() {
        let (store, warnings) = TaskStore::from_stored(None, None);
        assert!(store.tasks.is_empty());
        assert!(store.expansion.is_empty());
        assert!(warnings.is_empty());
    }

    /// 壊れた値は「空」として扱い、起動を止めない。`tasks.json` はエンドユーザーが
    /// 手で編集できる場所にあるので、これは異常系ではなく想定内の入力である。
    #[test]
    fn from_stored_survives_corrupt_values() {
        let (store, warnings) = TaskStore::from_stored(
            Some(serde_json::json!("これは配列ではない")),
            Some(serde_json::json!([1, 2, 3])),
        );
        assert!(store.tasks.is_empty());
        assert!(store.expansion.is_empty());
        assert_eq!(warnings.len(), 2);
    }

    /// タスクリストだけが壊れていても展開状態は残る (逆も同様)。境界が生き残れば
    /// 「消したタスクが復活する」ことは起きない。
    #[test]
    fn from_stored_isolates_the_two_keys() {
        let expansion = serde_json::json!({
            "t": { "expanded_until": 42, "schedule": "@hourly", "tz": null },
        });
        let (store, warnings) =
            TaskStore::from_stored(Some(serde_json::json!({"not": "a list"})), Some(expansion));
        assert!(store.tasks.is_empty());
        assert_eq!(store.expansion["t"].expanded_until, 42);
        assert_eq!(warnings.len(), 1);
    }

    /// 手で編集された `tasks.json` (順序が壊れている / id が重複している) を読んでも、
    /// due 取り出しの不変条件 (昇順・id 一意) が回復する。
    #[test]
    fn from_stored_restores_the_invariants() {
        let now = base();
        let tasks = serde_json::json!([
            { "id": "t@2", "origin": "schedule", "trigger_id": "t", "scheduled_at": now + DAY, "created_at": now },
            { "id": "t@1", "origin": "schedule", "trigger_id": "t", "scheduled_at": now, "created_at": now },
            { "id": "t@1", "origin": "schedule", "trigger_id": "t", "scheduled_at": now, "created_at": now },
        ]);
        let (store, warnings) = TaskStore::from_stored(Some(tasks), None);

        assert!(warnings.is_empty());
        assert_eq!(
            store
                .tasks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["t@1", "t@2"]
        );
        // 昇順が回復しているので、due は先頭からの走査で正しく取れる。
        assert_eq!(store.due(now).len(), 1);
    }

    /// `to_stored` → `from_stored` で内容が保存される。永続層に渡す 2 つの値が
    /// tasks / expansion に分かれていること自体が write amplification 対策の前提
    /// (#26 ストレージ判断)。
    #[test]
    fn stored_roundtrip_preserves_content() {
        let now = base();
        let mut store = TaskStore::default();
        store.insert(schedule_task("t", now + HOUR));
        store.expansion.insert(
            "t".into(),
            ExpansionState {
                expanded_until: now + DAY,
                schedule: "@hourly".into(),
                tz: Some("Asia/Tokyo".into()),
            },
        );

        let (tasks, expansion) = store.to_stored().unwrap();
        let (back, warnings) = TaskStore::from_stored(Some(tasks), Some(expansion));

        assert!(warnings.is_empty());
        assert_eq!(back.tasks.len(), 1);
        assert_eq!(back.tasks[0].scheduled_at, now + HOUR);
        assert_eq!(back.expansion["t"].expanded_until, now + DAY);
        assert_eq!(back.expansion["t"].tz.as_deref(), Some("Asia/Tokyo"));
    }
}
