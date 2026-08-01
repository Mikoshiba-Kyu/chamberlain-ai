import type { TaskListItem } from "../api";

interface Props {
  tasks: TaskListItem[];
  onDelete: (id: string) => void;
}

/**
 * 予定日時をローカル時刻で表示する。今日ぶんは時刻だけ、それ以外は日付も添える
 * (タスクリストは 48 時間先まで並ぶので「今日か明日か」が読めないと使えない)。
 */
export function formatScheduledAt(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => n.toString().padStart(2, "0");
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const today = new Date();
  const sameDay =
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate();
  if (sameDay) return time;
  return `${d.getMonth() + 1}/${d.getDate()} ${time}`;
}

/** 「あと N 分」の相対表示。過去 (= 心拍待ち or 遅延中) は「実行待ち」と出す。 */
export function formatRelative(ts: number): string {
  const diffMs = ts - Date.now();
  if (diffMs <= 0) return "実行待ち";
  const mins = Math.round(diffMs / 60000);
  if (mins < 60) return `あと ${mins}分`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `あと ${hours}時間${mins % 60}分`;
  return `あと ${Math.floor(hours / 24)}日${hours % 24}時間`;
}

export function TasksPanel({ tasks, onDelete }: Props) {
  // origin 別の内訳を出す。件数だけだと「@every 5m を入れたら 288 件になった」のような
  // 増え方の原因が読めないため (定期が膨らんだのか、依頼が溜まったのかを分ける)。
  const scheduleCount = tasks.filter((t) => t.origin === "schedule").length;
  const adhocCount = tasks.length - scheduleCount;

  return (
    <section className="panel">
      <h1>予定</h1>
      <p className="hint">
        秘書がこれから実行するつもりの予定です。<code>manifest.schedule</code>{" "}
        から展開されたものと、手動実行で積まれたものが同じリストに並びます。
        削除した予定が展開でよみがえることはありません。
      </p>
      {tasks.length > 0 ? (
        <div className="panel-summary">
          <span className="panel-summary-count">{tasks.length}</span>
          <span className="panel-summary-unit">件</span>
          <span className="panel-summary-detail">
            定期 {scheduleCount} / 手動・依頼 {adhocCount}
          </span>
        </div>
      ) : null}
      {tasks.length === 0 ? (
        <p className="placeholder">積まれている予定はありません。</p>
      ) : (
        <ul className="trigger-list">
          {tasks.map((t) => (
            <li key={t.id} className="trigger-row">
              <div className="trigger-meta">
                <div className="trigger-name">
                  {formatScheduledAt(t.scheduledAt)}
                  <span className="task-relative">
                    {formatRelative(t.scheduledAt)}
                  </span>
                </div>
                <div className="trigger-desc">
                  {t.triggerName ?? t.triggerId ?? "(対象トリガー無し)"}
                </div>
              </div>
              <div className="trigger-status">
                <span
                  className={
                    t.origin === "adhoc"
                      ? "status status-adhoc"
                      : "status status-schedule"
                  }
                >
                  {t.origin === "adhoc" ? "手動・依頼" : "定期"}
                </span>
                <button className="btn" onClick={() => onDelete(t.id)}>
                  削除
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
