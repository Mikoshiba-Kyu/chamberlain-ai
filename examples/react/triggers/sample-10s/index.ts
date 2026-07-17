// Chamberlain sample trigger.
//
// 永久タイマー (現状10s) のチェックポイントごとに Rust から tick() が呼ばれる。
// state はトリガーIDごとに namespace 分離された JSON として永続化される。
// - `ctx.state`: 前回 tick() が返した state (未保存なら {})
// - 戻り値の state が非 null なら丸ごと差し替わる (部分更新はスプレッドで自前)
// - notify を返すと OS通知 + UI アクティビティに流れる。title 省略時は manifest.name

type State = { tickCount?: number };

interface Ctx {
  now: number;
  state: State;
}

interface TickResult {
  notify?: { title?: string; body: string };
  state?: State;
}

export function tick(ctx: Ctx): TickResult | null {
  const next = (ctx.state.tickCount ?? 0) + 1;
  return {
    notify: { body: `Tick #${next}` },
    state: { tickCount: next },
  };
}
