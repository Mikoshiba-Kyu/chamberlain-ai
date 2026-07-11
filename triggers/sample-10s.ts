// Chamberlain sample trigger.
//
// このファイルはランタイムから `check()` が呼ばれるだけ。
// 5分チック等の永久タイマー (Rust側) の設計思想では、check() は
// "今この瞬間、通知するべきか / するなら何を言うか" を返すだけの
// 純粋な関数として書けるのが望ましい。
// PoC段階では戻り値スキーマは {message: string} | null に固定。

let tickCount = 0;

export function check(): { message: string } | null {
  tickCount += 1;
  return { message: `TS-driven tick #${tickCount}` };
}
