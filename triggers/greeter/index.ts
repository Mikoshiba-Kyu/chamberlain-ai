// 2つ目のサンプルトリガー。#8 の discovery / 独立ロード / 独立 pause を検証する。
//
// Ctx / TickResult の型は sample-10s と同じ形。将来的には framework パッケージから
// import する形になる (# TBD)。

type State = { greetCount?: number };

interface Ctx {
  now: number;
  state: State;
}

interface TickResult {
  notify?: { message: string };
  state?: State;
}

function greeting(now: number): string {
  const hour = new Date(now).getHours();
  if (hour < 6) return "夜遅くまで働いてる？";
  if (hour < 11) return "おはよう";
  if (hour < 17) return "こんにちは";
  if (hour < 22) return "こんばんは";
  return "そろそろ休もう";
}

export function tick(ctx: Ctx): TickResult | null {
  const next = (ctx.state.greetCount ?? 0) + 1;
  return {
    notify: { message: `${greeting(ctx.now)} (#${next})` },
    state: { greetCount: next },
  };
}
