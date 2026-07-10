import type { ActivityEvent } from "../api";

interface Props {
  events: ActivityEvent[];
}

function formatTime(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function ActivityPanel({ events }: Props) {
  return (
    <section className="panel">
      <h1>アクティビティ</h1>
      <p className="hint">
        すべてのトリガー発火・通知・提案の履歴です。OS の通知描画が届かない環境でも、ここに記録が残ります。
      </p>
      {events.length === 0 ? (
        <p className="placeholder">まだ何も起きていません。</p>
      ) : (
        <ul className="activity-log">
          {events.map((ev, i) => (
            <li key={`${ev.ts}-${i}`} className="activity-row">
              <span className="activity-time">{formatTime(ev.ts)}</span>
              <span className="activity-source">{ev.source}</span>
              <span className="activity-message">{ev.message}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
