import {
  chamberlainApi,
  type TriggerCandidate,
  type TriggerListItem,
  type TriggerSource,
} from "../api";

/**
 * トリガーを入れる前に見せるもの (#58 / #61)。
 *
 * **供給元で見せ方を変えない。** エンドユーザーが選んだフォルダも、秘書が書いた下書きも
 * (#61)、通る検証が同じなら読む画面も同じであるべきで、片方だけ簡略にすると「秘書が
 * 作ったものは確認しなくてよい」という読み方が生まれる。だから 2 つのパネルから同じ
 * コンポーネントを呼ぶ。
 */

/**
 * manifest に宣言された権限を 1 行にまとめる。何も宣言していなければ null。
 *
 * ここに出る文字列は**実際に効いている制限そのもの** (#56 / #57)。core が宣言の外を
 * 拒否するので、「宣言は飾りで実際は何でもできる」状態にはならない。
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

/**
 * 実行時間の宣言を 1 行にする (#81)。宣言していなければ null。
 *
 * **時間そのものより、宣言に付いてくる 2 つの約束の方が読み手には重い。** 長く走る
 * ということは、その間 AI 呼び出しが使う人の実費で積み上がりうるということで、
 * 落ちてもやり直されないということでもある。時間だけ出しても判断材料にならない。
 */
export function formatRuntime(
  t: Partial<Pick<TriggerCandidate, "maxRuntimeSec">>,
): string | null {
  // `?? null` は core の pin だけ古い状態への保険 (formatPermissions と同じ理由)。
  const secs = t.maxRuntimeSec ?? null;
  if (secs === null) {
    return null;
  }
  // 分に丸めるのは割り切れるときだけ。`Math.round` だと 150 秒が「最大 3 分」になり、
  // **宣言より長い時間を見せる**ことになる (同意画面に出す数字は宣言と一致させる)。
  const label =
    secs >= 120 && secs % 60 === 0
      ? `最大 ${secs / 60} 分`
      : `最大 ${secs} 秒`;
  return `${label}かかることがあります。長い実行の間 AI の利用料がかかることがあり、途中でアプリが終了した場合はやり直されません。`;
}

/** id が既存とぶつかっているときの注意書き。ぶつかっていなければ null。 */
export function describeConflict(
  conflict: TriggerSource | null,
): string | null {
  if (conflict === "bundled") {
    return "同じ id のトリガーがアプリに同梱されています。同梱された方が優先されるため、登録できません。";
  }
  if (conflict === "registered") {
    return "同じ id のトリガーが既に登録されています。登録すると置き換わります。";
  }
  return null;
}

interface Props {
  candidate: TriggerCandidate;
  /**
   * 見出しの文言。**呼び出し側が決める。**
   *
   * 「秘書が作ったものか」を示す prop にしないのは、出どころを知っているのが呼び出し側
   * だから。このコンポーネントに出どころを渡すと、次に「秘書のときは警告を畳む」のような
   * 分岐を足す場所ができてしまう — 出どころで見せ方を変えないことがこの画面の役目。
   */
  heading?: string;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConsentCard({
  candidate,
  heading = "このトリガーを登録しますか？",
  busy,
  onConfirm,
  onCancel,
}: Props) {
  const conflictNote = describeConflict(candidate.conflict);
  const runtimeNote = formatRuntime(candidate);
  // core の pin だけ古い状態でも落ちないように (formatPermissions と同じ理由)。
  const warnings = candidate.warnings ?? [];

  return (
    <div className="consent">
      <h2>{heading}</h2>
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
        {runtimeNote && (
          <>
            <dt>実行時間</dt>
            <dd className="consent-runtime">{runtimeNote}</dd>
          </>
        )}
        <dt>場所</dt>
        <dd className="consent-path">{candidate.path}</dd>
      </dl>
      <p className="hint">
        ここに出ている宣言が、このトリガーにできることのすべてです。宣言していない鍵は読めず、
        宣言していない宛先には出られません。
      </p>
      {warnings.length > 0 && (
        <ul className="consent-warning consent-warnings">
          {warnings.map((w) => (
            <li key={w}>{w}</li>
          ))}
        </ul>
      )}
      {conflictNote && <p className="consent-warning">{conflictNote}</p>}
      <div className="consent-actions">
        <button
          className="btn"
          onClick={onConfirm}
          disabled={busy || candidate.conflict === "bundled"}
        >
          登録する
        </button>
        <button className="btn" onClick={onCancel} disabled={busy}>
          やめる
        </button>
      </div>
    </div>
  );
}

/**
 * 登録の反映待ちを知らせる帯 (#58)。**登録できる画面すべてに要る** — チャットから
 * 入れた (#61) ときに案内が出ないと、「登録したのに動かない」で終わる。
 */
export function RestartNotice() {
  return (
    <div className="notice notice-action">
      <span>
        登録したトリガーは再起動後に動き始めます (解除は再起動を待ちません)。
      </span>
      <button className="btn" onClick={() => chamberlainApi.restartApp()}>
        再起動する
      </button>
    </div>
  );
}
