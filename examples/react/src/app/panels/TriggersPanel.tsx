import { useState } from "react";
import {
  chamberlainApi,
  type TriggerCandidate,
  type TriggerListItem,
  type TriggerSource,
} from "../api";

interface Props {
  triggers: TriggerListItem[];
  onToggle: (id: string) => void;
  onRunNow: (id: string) => void;
  /** 解除でトリガー一覧が変わったときに呼ぶ (App が取り直す)。 */
  onChanged: () => void;
  /**
   * 登録済みだがまだ反映されていないものがあるか (#58)。
   *
   * **このパネルの state ではなく App が持つ。** タブを切り替えるとパネルは unmount
   * されるので、ここに置くと「入れたのに一覧に出ず、案内も消えた」状態が作れてしまう。
   */
  restartPending: boolean;
  onRegistered: () => void;
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
 * 拒否するので、「宣言は飾りで実際は何でもできる」状態にはならない。実行時登録 (#58) の
 * 同意画面はこれと同じものを、入れる前に見せる。
 */
export function formatPermissions(
  t: Partial<Pick<TriggerListItem, "requiredSecrets" | "allowedHosts">>,
): string | null {
  // `?? []` は core の pin だけ古い状態への保険。release.yml が pin を機械で検査して
  // いるが、手で戻された場合でも画面が真っ白にはならないようにする。
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

/** トリガーの出どころのラベル (#58)。 */
export function sourceLabel(source: TriggerSource): string {
  return source === "registered" ? "登録" : "同梱";
}

/**
 * 同意画面に出す権限の行。**何も宣言していないことを空欄にしない。**
 *
 * 「宣言なし」は「制限なし」ではなく「鍵もネットワークも使えない」という強い意味を持つ
 * (#56 / #57)。空欄だと読み手が逆に取る。
 */
export function formatConsentPermissions(
  candidate: Pick<TriggerCandidate, "requiredSecrets" | "allowedHosts">,
): string {
  return (
    formatPermissions(candidate) ?? "鍵もネットワークも使いません (宣言なし)"
  );
}

/** id が既存とぶつかっているときの注意書き。ぶつかっていなければ null。 */
export function describeConflict(conflict: TriggerSource | null): string | null {
  if (conflict === "bundled") {
    return "同じ id のトリガーがアプリに同梱されています。同梱された方が優先されるため、登録できません。";
  }
  if (conflict === "registered") {
    return "同じ id のトリガーが既に登録されています。登録すると置き換わります。";
  }
  return null;
}

export function TriggersPanel({
  triggers,
  onToggle,
  onRunNow,
  onChanged,
  restartPending,
  onRegistered,
}: Props) {
  // 下見が終わって同意待ちのトリガー。null の間は登録の口が閉じている。
  const [candidate, setCandidate] = useState<TriggerCandidate | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmingUnregister, setConfirmingUnregister] = useState<string | null>(
    null,
  );

  /** 登録系の操作はどれも「二重に押させない・失敗を画面に出す」が要る。 */
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const pickFolder = () =>
    run(async () => {
      const picked = await chamberlainApi.pickTriggerFolder();
      // null = キャンセル。画面は何も変えない。
      if (picked) setCandidate(picked);
    });

  const confirmRegister = () =>
    run(async () => {
      if (!candidate) return;
      const registered = await chamberlainApi.registerTrigger(candidate.path);
      setCandidate(null);
      // 一覧は取り直さない。反映は再起動からなので、今 list_triggers を読んでも
      // 同じものが返る (#58)。代わりに「再起動待ち」を App に預ける。
      onRegistered();
      setNotice(`${registered.name} (${registered.id}) を登録しました。`);
    });

  const saveSkill = () =>
    run(async () => {
      const saved = await chamberlainApi.saveTriggerSkill();
      // null = キャンセル。画面は何も変えない。
      if (saved) setNotice(`${saved} に書き出しました。`);
    });

  const unregister = (id: string) =>
    run(async () => {
      setConfirmingUnregister(null);
      await chamberlainApi.unregisterTrigger(id);
      setNotice(`${id} を解除しました。積まれていた予定も消えています。`);
      onChanged();
    });

  const conflictNote = candidate ? describeConflict(candidate.conflict) : null;

  return (
    <section className="panel">
      <h1>トリガー</h1>
      <p className="hint">
        <code>triggers/&lt;id&gt;/manifest.json</code> と <code>index.ts</code>{" "}
        から自動検出されたトリガーの一覧です。「次の予定」は予定リストの投影なので、
        予定を削除すると消えます。
      </p>

      <div className="trigger-toolbar">
        <button className="btn" onClick={pickFolder} disabled={busy}>
          フォルダから追加…
        </button>
        <button className="btn" onClick={saveSkill} disabled={busy}>
          書き方を skill として保存…
        </button>
        <span className="hint">
          <code>manifest.json</code>{" "}
          があるフォルダを選ぶと、そのトリガーが何を読み・どこへ出るのかを確認してから登録します。
          新しく作りたいときは、トリガーの書き方を skill として書き出して AI
          に渡すと、フォルダごと作ってもらえます。
        </span>
      </div>

      {error && <p className="error">エラー: {error}</p>}
      {notice && <p className="notice">{notice}</p>}
      {restartPending && (
        <div className="notice notice-action">
          <span>
            登録したトリガーは再起動後に動き始めます (解除は再起動を待ちません)。
          </span>
          <button className="btn" onClick={() => chamberlainApi.restartApp()}>
            再起動する
          </button>
        </div>
      )}

      {candidate && (
        <div className="consent">
          <h2>このトリガーを登録しますか？</h2>
          <dl className="consent-fields">
            <dt>ID</dt>
            <dd>
              <code>{candidate.id}</code>
            </dd>
            <dt>名前</dt>
            <dd>
              {candidate.name}
              {candidate.description ? ` — ${candidate.description}` : ""}
            </dd>
            <dt>実行タイミング</dt>
            <dd>
              <code>{candidate.schedule}</code>
              {candidate.tz ? ` (${candidate.tz})` : ""}
            </dd>
            <dt>できること</dt>
            <dd>{formatConsentPermissions(candidate)}</dd>
            <dt>場所</dt>
            <dd className="consent-path">{candidate.path}</dd>
          </dl>
          <p className="hint">
            ここに出ている宣言が、このトリガーにできることのすべてです。宣言していない鍵は読めず、
            宣言していない宛先には出られません。
          </p>
          {conflictNote && <p className="consent-warning">{conflictNote}</p>}
          <div className="consent-actions">
            <button
              className="btn"
              onClick={confirmRegister}
              disabled={busy || candidate.conflict === "bundled"}
            >
              登録する
            </button>
            <button
              className="btn"
              onClick={() => setCandidate(null)}
              disabled={busy}
            >
              やめる
            </button>
          </div>
        </div>
      )}

      {triggers.length === 0 ? (
        <p className="placeholder">検出されたトリガーはありません。</p>
      ) : (
        <ul className="trigger-list">
          {triggers.map((t) => {
            const permissions = formatPermissions(t);
            return (
              <li key={t.id} className="trigger-row">
                <div className="trigger-meta">
                  <div className="trigger-name">
                    {t.id}
                    <span className={`status status-source-${t.source}`}>
                      {sourceLabel(t.source)}
                    </span>
                  </div>
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
                  <TriggerActions
                    trigger={t}
                    busy={busy}
                    confirming={confirmingUnregister === t.id}
                    onToggle={onToggle}
                    onRunNow={onRunNow}
                    onAskUnregister={setConfirmingUnregister}
                    onUnregister={unregister}
                  />
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

interface ActionsProps {
  trigger: TriggerListItem;
  busy: boolean;
  /** 解除の確認待ちか。確認中は他の操作を出さない (押し間違いを防ぐ)。 */
  confirming: boolean;
  onToggle: (id: string) => void;
  onRunNow: (id: string) => void;
  onAskUnregister: (id: string | null) => void;
  onUnregister: (id: string) => void;
}

/**
 * 行の右側。状態は「解除の確認中 / 構成エラー / 通常」の 3 つで、互いに排他なので
 * 早期 return で並べる (入れ子の三項演算子にすると 4 つ目を足せなくなる)。
 */
function TriggerActions({
  trigger: t,
  busy,
  confirming,
  onToggle,
  onRunNow,
  onAskUnregister,
  onUnregister,
}: ActionsProps) {
  if (confirming) {
    return (
      <>
        <span className="status status-error">解除しますか？</span>
        <button className="btn" onClick={() => onUnregister(t.id)} disabled={busy}>
          解除する
        </button>
        <button className="btn" onClick={() => onAskUnregister(null)}>
          やめる
        </button>
      </>
    );
  }

  // 外せるのは登録したものだけ。同梱は「アプリの形」の一部 (#55)。
  const unregisterButton = t.source === "registered" && (
    <button
      className="btn"
      onClick={() => onAskUnregister(t.id)}
      disabled={busy}
    >
      解除
    </button>
  );

  if (t.error) {
    return (
      <>
        <span className="status status-error">構成エラー</span>
        {unregisterButton}
      </>
    );
  }

  return (
    <>
      <span
        className={t.paused ? "status status-paused" : "status status-running"}
      >
        {t.paused ? "停止中" : "実行中"}
      </span>
      <button className="btn" onClick={() => onRunNow(t.id)}>
        今すぐ実行
      </button>
      <button className="btn" onClick={() => onToggle(t.id)}>
        {t.paused ? "再開" : "停止"}
      </button>
      {unregisterButton}
    </>
  );
}
