// 朝 06:00 の挨拶。#18 で greeter を 4 分割した 1 つ目。
//
// wall-clock schedule (`@daily 06:00`) が実際に指定時刻で fire することを
// 検証するのが目的。tick 内での時間帯 dispatch (旧 greeter の実装) は消え、
// トリガー = 時間帯 という 1:1 の分割になっている。

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
