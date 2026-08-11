// 朝 06:00 の挨拶。wall-clock schedule (`@daily 06:00`) の実運用サンプル (#18)。
//
// トリガー = 時間帯 の 1:1 対応にしてあり、tick 側で時間帯を判定することはしない。

type State = { greetCount?: number };

interface Ctx {
  now: number;
  state: State;
}

interface TickResult {
  notify?: { title?: string; body: string };
  state?: State;
}

export function tick(ctx: Ctx): TickResult {
  const next = (ctx.state.greetCount ?? 0) + 1;
  return {
    notify: { body: `おはよう (#${next})` },
    state: { greetCount: next },
  };
}
