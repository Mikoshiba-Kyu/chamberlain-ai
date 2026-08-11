// 昼 10:00 の挨拶。

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
    notify: { body: `こんにちは (#${next})` },
    state: { greetCount: next },
  };
}
