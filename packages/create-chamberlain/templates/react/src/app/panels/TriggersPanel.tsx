import type { TriggerListItem } from "../api";

interface Props {
  triggers: TriggerListItem[];
  onToggle: (id: string) => void;
  onRunNow: (id: string) => void;
}

/** 次の予定時刻をローカル時刻で表示する。null は「積まれていない」。 */
function formatNextFireAt(ts: number | null): string {
  if (ts === null) return "予定なし";
  const d = new Date(ts);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function TriggersPanel({ triggers, onToggle, onRunNow }: Props) {
  return (
    <section className="panel">
      <h1>トリガー</h1>
      <p className="hint">
        <code>triggers/&lt;id&gt;/manifest.json</code> と <code>index.ts</code>{" "}
        から自動検出されたトリガーの一覧です。「次の予定」は予定リストの投影なので、
        予定を削除すると消えます。
      </p>
      {triggers.length === 0 ? (
        <p className="placeholder">検出されたトリガーはありません。</p>
      ) : (
        <ul className="trigger-list">
          {triggers.map((t) => (
            <li key={t.id} className="trigger-row">
              <div className="trigger-meta">
                <div className="trigger-name">{t.id}</div>
                <div className="trigger-desc">
                  {t.name}
                  {t.description ? ` — ${t.description}` : ""}
                </div>
                <div className="trigger-schedule">
                  <code>{t.schedule}</code>
                  {t.error ? null : (
                    <span> · 次の予定 {formatNextFireAt(t.nextFireAt)}</span>
                  )}
                </div>
                {t.error ? (
                  <div className="trigger-error">エラー: {t.error}</div>
                ) : null}
              </div>
              <div className="trigger-status">
                {t.error ? (
                  <span className="status status-error">構成エラー</span>
                ) : (
                  <>
                    <span
                      className={
                        t.paused
                          ? "status status-paused"
                          : "status status-running"
                      }
                    >
                      {t.paused ? "停止中" : "実行中"}
                    </span>
                    <button className="btn" onClick={() => onRunNow(t.id)}>
                      今すぐ実行
                    </button>
                    <button className="btn" onClick={() => onToggle(t.id)}>
                      {t.paused ? "再開" : "停止"}
                    </button>
                  </>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
