import { describe, expect, it } from "vitest";

import { formatNextFireAt, sourceLabel } from "./TriggersPanel";

// TZ は Asia/Tokyo に固定してある (vite.config.ts の `test.env`)。
describe("formatNextFireAt", () => {
  it("null は「予定なし」", () => {
    // nextFireAt はタスクリストの投影なので、予定を全部消せば null になる (#26 決定事項 2)。
    expect(formatNextFireAt(null)).toBe("予定なし");
  });

  it("日付と時刻をローカル時刻で出す", () => {
    // 00:00 UTC は JST の 09:00。
    expect(formatNextFireAt(new Date("2026-01-01T00:00:00Z").getTime())).toBe(
      "1/1 09:00",
    );
  });

  it("時と分をゼロ埋めする (月日はしない)", () => {
    expect(formatNextFireAt(new Date("2026-03-04T20:05:00Z").getTime())).toBe(
      "3/5 05:05",
    );
  });
});

// 実行時登録 (#58)。宣言の見せ方は同意画面と共有しているので `ConsentCard.test.ts`。
describe("sourceLabel", () => {
  it("登録と同梱を区別する", () => {
    expect(sourceLabel("registered")).toBe("登録");
    expect(sourceLabel("bundled")).toBe("同梱");
  });
});
