// 夕方 17:00 の挨拶。#18 で greeter を 4 分割した 3 つ目。

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
    notify: { body: `こんばんは (#${next})` },
    state: { greetCount: next },
  };
}
