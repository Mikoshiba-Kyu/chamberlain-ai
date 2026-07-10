interface Props {
  paused: boolean;
  onToggle: () => void;
}

export function TriggersPanel({ paused, onToggle }: Props) {
  return (
    <section className="panel">
      <h1>トリガー</h1>
      <p className="hint">
        開発者が定義したトリガー（いつ／何を確認して／何を通知するか）の一覧です。MVP では 1 件のサンプルのみ。
      </p>
      <ul className="trigger-list">
        <li className="trigger-row">
          <div className="trigger-meta">
            <div className="trigger-name">sample-10s</div>
            <div className="trigger-desc">10秒ごとに通知を発火するサンプル</div>
          </div>
          <div className="trigger-status">
            <span className={paused ? "status status-paused" : "status status-running"}>
              {paused ? "停止中" : "実行中"}
            </span>
            <button className="btn" onClick={onToggle}>
              {paused ? "再開" : "停止"}
            </button>
          </div>
        </li>
      </ul>
    </section>
  );
}
