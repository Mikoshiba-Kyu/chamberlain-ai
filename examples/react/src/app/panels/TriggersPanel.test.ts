import { describe, expect, it } from "vitest";

import {
  describeConflict,
  formatConsentPermissions,
  formatNextFireAt,
  formatPermissions,
  sourceLabel,
} from "./TriggersPanel";

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

// 宣言は core が実際に強制している内容そのもの (#56 / #57)。表示が宣言と食い違うと
// 「同意画面は出ているが効いていない」になるので、組み立てだけは固定しておく。
describe("formatPermissions", () => {
  it("何も宣言していなければ行を出さない", () => {
    expect(
      formatPermissions({ requiredSecrets: [], allowedHosts: [] }),
    ).toBeNull();
  });

  it("鍵と宛先を両方出す", () => {
    expect(
      formatPermissions({
        requiredSecrets: ["github_token"],
        allowedHosts: ["api.github.com", "*.githubusercontent.com"],
      }),
    ).toBe("鍵 github_token · 宛先 api.github.com, *.githubusercontent.com");
  });

  it("フィールドが無い古い core でも落ちない", () => {
    // pin を戻された等でフロントだけ新しい状態。画面が真っ白になるより無表示にする。
    expect(formatPermissions({})).toBeNull();
  });

  it("片方だけの宣言でも区切りが余らない", () => {
    expect(
      formatPermissions({ requiredSecrets: [], allowedHosts: ["example.com"] }),
    ).toBe("宛先 example.com");
    expect(
      formatPermissions({ requiredSecrets: ["token"], allowedHosts: [] }),
    ).toBe("鍵 token");
  });
});

// 実行時登録 (#58)。エンドユーザーが「入れる前に」読む唯一の画面なので、宣言の見せ方と
// 衝突の言い方だけは固定しておく。
describe("sourceLabel", () => {
  it("登録と同梱を区別する", () => {
    expect(sourceLabel("registered")).toBe("登録");
    expect(sourceLabel("bundled")).toBe("同梱");
  });
});

describe("formatConsentPermissions", () => {
  it("宣言があればそのまま出す", () => {
    expect(
      formatConsentPermissions({
        requiredSecrets: ["github_token"],
        allowedHosts: ["api.github.com"],
      }),
    ).toBe("鍵 github_token · 宛先 api.github.com");
  });

  it("宣言が無いことを空欄にしない", () => {
    // 「宣言なし」は「制限なし」ではなく「何もできない」。空欄だと逆に読める。
    expect(
      formatConsentPermissions({ requiredSecrets: [], allowedHosts: [] }),
    ).toBe("鍵もネットワークも使いません (宣言なし)");
  });
});

describe("describeConflict", () => {
  it("同梱との衝突は登録できないと言う", () => {
    expect(describeConflict("bundled")).toContain("登録できません");
  });

  it("登録済みとの衝突は置き換えになると言う", () => {
    expect(describeConflict("registered")).toContain("置き換わります");
  });

  it("衝突が無ければ何も出さない", () => {
    expect(describeConflict(null)).toBeNull();
  });
});
