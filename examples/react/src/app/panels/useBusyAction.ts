import { useState } from "react";

/**
 * 「二重に押させない・失敗を画面に出す」を 1 箇所に閉じる。
 *
 * トリガーを入れる操作はトリガー画面 (#58) とチャット (#61) の 2 箇所にあり、どちらも
 * 同じ扱いが要る。**パネルごとに書くと片方だけ直る** — 同じ同意画面 (`ConsentCard`) を
 * 出しているのに、押したあとの振る舞いが割れることになる。
 */
export function useBusyAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  /** 走らせている間は busy。失敗は握り潰さず画面に出す。 */
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

  return { busy, error, notice, setError, setNotice, run };
}
