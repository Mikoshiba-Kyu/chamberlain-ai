import type { TriggerListItem } from "../api";

interface Props {
  triggers: TriggerListItem[];
  onToggle: (id: string) => void;
  onRunNow: (id: string) => void;
}

/** 次の予定時刻をローカル時刻で表示する。null は「積まれていない」。 */
export function formatNextFireAt(ts: number | null): string {
  if (ts === null) return "予定なし";
  const d = new Date(ts);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * manifest に宣言された権限を 1 行にまとめる。何も宣言していなければ null。
 *
 * ここに出る文字列は**実際に効いている制限そのもの** (#56 / #57)。core が宣言の外を
 * 拒否するので、「宣言は飾りで実際は何でもできる」状態にはならない。実行時登録 (#55) の
 * 同意画面はこれと同じものを、入れる前に見せることになる。
 */
export function formatPermissions(
  t: Partial<Pick<TriggerListItem, "requiredSecrets" | "allowedHosts">>,
): string | null {
  // `?? []` は 0.2.x の core を引いたままフロントだけ新しい場合の保険。
  // リリースは lockstep で、テンプレの `chamberlain-core` の pin は release.yml が
  // 機械で検査しているが、手で pin を戻された状態でも画面が真っ白にはならないようにする。
  const secrets = t.requiredSecrets ?? [];
  const hosts = t.allowedHosts ?? [];
  const parts: string[] = [];
  if (secrets.length > 0) {
    parts.push(`鍵 ${secrets.join(", ")}`);
  }
  if (hosts.length > 0) {
    parts.push(`宛先 ${hosts.join(", ")}`);
  }
  return parts.length > 0 ? parts.join(" · ") : null;
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
          {triggers.map((t) => {
            const permissions = formatPermissions(t);
            return (
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
                  {permissions ? (
                    <div className="trigger-permissions">{permissions}</div>
                  ) : null}
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
            );
          })}
        </ul>
      )}
    </section>
  );
}
