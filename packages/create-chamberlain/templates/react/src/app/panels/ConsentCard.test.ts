import { describe, expect, it } from "vitest";

import {
  describeConflict,
  formatConsentPermissions,
  formatPermissions,
  formatRuntime,
  formatTokenBudget,
} from "./ConsentCard";

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

// 実行時登録 (#58) と秘書の生成 (#61)。エンドユーザーが「入れる前に」読む唯一の画面
// なので、宣言の見せ方と衝突の言い方だけは固定しておく。
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

// #81。宣言していないトリガーと同じ画面に並ぶので、**違いが読み取れること**だけを固定する。
describe("formatRuntime", () => {
  it("宣言が無ければ何も出さない", () => {
    // 既定 (110 秒) は今までどおりなので、わざわざ言うことが無い。
    expect(formatRuntime({ maxRuntimeSec: null })).toBeNull();
  });

  it("core の pin が古くて欠けていても落ちない", () => {
    expect(formatRuntime({})).toBeNull();
  });

  it("時間だけでなく、課金とやり直しの約束も言う", () => {
    const note = formatRuntime({ maxRuntimeSec: 1800 });
    expect(note).toContain("30 分");
    expect(note).toContain("利用料");
    expect(note).toContain("やり直されません");
  });

  it("2 分未満は秒のまま出す", () => {
    expect(formatRuntime({ maxRuntimeSec: 111 })).toContain("111 秒");
  });

  it("割り切れない秒数を分に丸めない (宣言より長く見せない)", () => {
    expect(formatRuntime({ maxRuntimeSec: 150 })).toContain("150 秒");
  });
});

// #82。**承認する前に、増える消費が見える**ようにするための行なので、掛け算と
// 「月あたりが無い場合」だけ固定する。
describe("formatTokenBudget", () => {
  it("1 回あたりと月あたりを掛け算して出す", () => {
    const note = formatTokenBudget({
      maxTokensPerRun: 24576,
      runsPerMonth: 8928,
    });
    expect(note).toContain("24,576");
    expect(note).toContain("8,928");
    expect(note).toContain("219,414,528");
  });

  it("1 回きりのトリガーに月あたりを出さない", () => {
    // `@at` に「月あたり 0 回」と書くと AI を使わないように読める。
    const note = formatTokenBudget({ maxTokensPerRun: 4096, runsPerMonth: null });
    expect(note).toContain("1 回だけ");
    expect(note).not.toContain("月あたり");
  });

  it("金額には換算しない", () => {
    // 単価を持つと、その表の陳腐化を追い続けることになる (#71 / #27)。
    const note = formatTokenBudget({
      maxTokensPerRun: 24576,
      runsPerMonth: 31,
    });
    expect(note).not.toMatch(/[¥$]/);
    expect(note).toContain("単価");
  });

  it("32bit を超える見積もりでも桁が壊れない", () => {
    // core は u32 なのでここで掛ける。飽和させると天井が実際より小さく見える。
    const note = formatTokenBudget({
      maxTokensPerRun: 4122880,
      runsPerMonth: 8928,
    });
    expect(note).toContain("36,809,072,640");
  });

  it("見積もりが出せないトリガーには何も出さない", () => {
    // 構成エラーのトリガーは core が両方 null で返す (#82)。UI 側で `error` を
    // 見に行かないための約束なので、ここで固定しておく。
    expect(
      formatTokenBudget({ maxTokensPerRun: null, runsPerMonth: null }),
    ).toBeNull();
    // core の pin だけ古くてフィールドごと欠けている場合も同じ。
    expect(formatTokenBudget({})).toBeNull();
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
