// Chamberlain sample trigger.
//
// 永久タイマー (現状10s) のチェックポイントごとに Rust から check() が呼ばれる。
// state はトリガーIDごとに namespace 分離された JSON として永続化される。
// - `ctx.state`: 前回 check() が返した state (未保存なら {})
// - 戻り値の state が非 null なら丸ごと差し替わる (部分更新はスプレッドで自前)
// - notify を返すと OS通知 + UI アクティビティに流れる

type State = { tickCount?: number };

interface Ctx {
  now: number;
  state: State;
}

interface CheckResult {
  notify?: { message: string };
  state?: State;
}

export function check(ctx: Ctx): CheckResult | null {
  const next = (ctx.state.tickCount ?? 0) + 1;
  return {
    notify: { message: `Tick #${next}` },
    state: { tickCount: next },
  };
}
