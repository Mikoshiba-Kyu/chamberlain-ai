import { useState } from "react";
import {
  chamberlainApi,
  type TriggerCandidate,
  type TriggerListItem,
  type TriggerSource,
} from "../api";
import {
  ConsentCard,
  RestartNotice,
  formatPermissions,
  formatRuntime,
  formatTokenBudget,
} from "./ConsentCard";
import { useBusyAction } from "./useBusyAction";

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

/** トリガーの出どころのラベル (#58)。 */
export function sourceLabel(source: TriggerSource): string {
  return source === "registered" ? "登録" : "同梱";
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
  const [confirmingUnregister, setConfirmingUnregister] = useState<
    string | null
  >(null);
  const { busy, error, notice, setNotice, run } = useBusyAction();

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
      {restartPending && <RestartNotice />}

      {candidate && (
        <ConsentCard
          candidate={candidate}
          busy={busy}
          onConfirm={confirmRegister}
          onCancel={() => setCandidate(null)}
        />
      )}

      {triggers.length === 0 ? (
        <p className="placeholder">検出されたトリガーはありません。</p>
      ) : (
        <ul className="trigger-list">
          {triggers.map((t) => {
            const permissions = formatPermissions(t);
            // 同意画面と同じ文言を出す (#81)。**同梱トリガーはここにしか出ない** —
            // 確認画面を通らないので、長く走る / やり直されないことを読める場所が
            // 一覧のほかに無い。
            const runtime = formatRuntime(t);
            // 消費の見積もりも同意画面と同じものを出す (#82)。**同梱トリガーは
            // 確認画面を通らない**ので、ここに出さないとアプリに元から入っている
            // トリガーの消費だけがどこからも読めない。構成エラーのトリガーを外すのは
            // core の仕事で、その場合は null が返る。
            const budget = formatTokenBudget(t);
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
                  {runtime ? (
                    <div className="consent-runtime">{runtime}</div>
                  ) : null}
                  {budget ? (
                    <div className="consent-budget">{budget}</div>
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
        <button
          className="btn"
          onClick={() => onUnregister(t.id)}
          disabled={busy}
        >
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
