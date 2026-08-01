import { afterEach, describe, expect, it, vi } from "vitest";

import { formatRelative, formatScheduledAt } from "./TasksPanel";

// 時刻の表示はローカル TZ に依存する。テストは TZ を Asia/Tokyo に固定した上で
// (vite.config.ts の `test.env`)、「今」も固定して回す。
const JST = (iso: string) => new Date(iso).getTime();

afterEach(() => {
  vi.useRealTimers();
});

/** 「今」を固定する。予定表示は現在時刻との関係で出し分けるため。 */
function freezeNow(iso: string) {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(iso));
}

describe("formatScheduledAt", () => {
  it("今日の予定は時刻だけを出す", () => {
    freezeNow("2026-01-01T03:00:00Z"); // 12:00 JST
    expect(formatScheduledAt(JST("2026-01-01T09:30:00Z"))).toBe("18:30");
  });

  it("別の日の予定は日付も添える (48h 先まで並ぶので今日か明日かが読めないと使えない)", () => {
    freezeNow("2026-01-01T03:00:00Z");
    expect(formatScheduledAt(JST("2026-01-02T09:05:00Z"))).toBe("1/2 18:05");
  });

  it("UTC ではなくローカル時刻で表示する", () => {
    // 00:00 UTC は JST の 09:00。UTC のまま出していれば "00:00" になる。
    freezeNow("2026-01-01T00:00:00Z");
    expect(formatScheduledAt(JST("2026-01-01T00:00:00Z"))).toBe("09:00");
  });

  it("日付をまたぐと同じ時刻でも表示が変わる", () => {
    // 2026-01-01T16:00Z = 1/2 01:00 JST。JST 基準では「今日」ではない。
    freezeNow("2026-01-01T03:00:00Z");
    expect(formatScheduledAt(JST("2026-01-01T16:00:00Z"))).toBe("1/2 01:00");
  });

  it("時と分をゼロ埋めする (月日はしない)", () => {
    freezeNow("2026-01-01T00:00:00Z"); // 1/1 09:00 JST
    expect(formatScheduledAt(JST("2026-01-01T20:01:00Z"))).toBe("1/2 05:01");
  });
});

describe("formatRelative", () => {
  it("過ぎた予定は「実行待ち」(心拍待ち or 遅延中)", () => {
    freezeNow("2026-01-01T00:00:00Z");
    expect(formatRelative(JST("2025-12-31T23:59:00Z"))).toBe("実行待ち");
    expect(formatRelative(JST("2026-01-01T00:00:00Z"))).toBe("実行待ち");
  });

  it("1 時間未満は分で出す", () => {
    freezeNow("2026-01-01T00:00:00Z");
    expect(formatRelative(JST("2026-01-01T00:05:00Z"))).toBe("あと 5分");
    expect(formatRelative(JST("2026-01-01T00:59:00Z"))).toBe("あと 59分");
  });

  it("1 日未満は時間と分で出す", () => {
    freezeNow("2026-01-01T00:00:00Z");
    expect(formatRelative(JST("2026-01-01T01:30:00Z"))).toBe("あと 1時間30分");
    expect(formatRelative(JST("2026-01-01T23:00:00Z"))).toBe("あと 23時間0分");
  });

  it("1 日以上は日と時間で出す (展開ホライズンは 48h なのでここまで出る)", () => {
    freezeNow("2026-01-01T00:00:00Z");
    expect(formatRelative(JST("2026-01-02T03:00:00Z"))).toBe("あと 1日3時間");
    expect(formatRelative(JST("2026-01-02T23:00:00Z"))).toBe("あと 1日23時間");
  });
});
