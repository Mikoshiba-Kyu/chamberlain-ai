// #15: Type I トリガー (トリガー内タスク AI) の初実装。
//
// 動作:
//   1. `github_token` を secret store から取る (未設定なら notify で誘導して終了)
//   2. GitHub Search API で対象リポの open/closed Issue 数を取る
//      (Search API は PR を is:issue で除外できる。REST /repos の open_issues_count は
//       PR も含んでしまうので使わない)
//   3. `chamberlain.ai.complete` に count を渡して短い一言コメントを生成
//   4. notify で { title, body } を返す
//
// スコープ外 (#15 の記述通り):
//   - 対象 repo 指定の manifest 拡張 (今はここで hardcode)
//   - cadence 論点 (現状 core が 10s 毎に tick、それに従う。Search API rate limit
//     30/min にすぐ引っかかるが、動作検証はそれで足りる)
//   - 複数 repo / Draft / assigned などの詳細集計

// 対象リポは Chamberlain 自身。実データで動くのが検証しやすい。
const REPO = "Mikoshiba-Kyu/chamberlain-ai";

// ---- framework 側から注入される globals の型宣言 ----
// bootstrap.js が globalThis.chamberlain を生やしている。
// 型は将来 core パッケージから import する形になる (別 Issue、# TBD)。
declare const chamberlain: {
  // manifest.json の requiredSecrets に書いた名前しか返らない (#56)。宣言外は null が
  // 返り、活動ログに [denied] が出る。このトリガーは "github_token" を宣言している。
  getSecret(name: string): Promise<string | null>;
  ai: {
    complete(opts: { prompt: string; system?: string; model?: string }): Promise<string>;
  };
  http: {
    // manifest.json の allowedHosts に書いたホストにしか出られない (#57)。https のみ
    // (平文はループバックだけ)。リダイレクトはホップごとに同じ宣言と照合される。
    // このトリガーは "api.github.com" を宣言している。
    fetch(
      url: string,
      opts?: {
        method?: string;
        headers?: Record<string, string>;
        body?: string;
      }
    ): Promise<{ status: number; body: string }>;
  };
};

interface Ctx {
  now: number;
  state: Record<string, unknown>;
}

interface TickResult {
  notify?: { title?: string; body: string };
  state?: Record<string, unknown>;
}

async function searchIssueCount(
  token: string,
  repo: string,
  state: "open" | "closed"
): Promise<number> {
  // Search API は `is:issue` で PR を除外できる。per_page=1 で件数だけ取る。
  const q = encodeURIComponent(`repo:${repo} is:issue is:${state}`);
  const url = `https://api.github.com/search/issues?q=${q}&per_page=1`;
  const resp = await chamberlain.http.fetch(url, {
    method: "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      // GitHub API は User-Agent 必須
      "User-Agent": "chamberlain-ai-trigger",
    },
  });
  if (resp.status !== 200) {
    throw new Error(`GitHub API ${resp.status}: ${resp.body.slice(0, 200)}`);
  }
  const parsed = JSON.parse(resp.body) as { total_count?: number };
  if (typeof parsed.total_count !== "number") {
    throw new Error(`unexpected response shape: ${resp.body.slice(0, 200)}`);
  }
  return parsed.total_count;
}

export async function tick(_ctx: Ctx): Promise<TickResult | null> {
  const token = await chamberlain.getSecret("github_token");
  if (!token) {
    return {
      notify: {
        body: "GitHub トークン (github_token) が未設定です。設定画面から登録してください。",
      },
    };
  }

  let openCount: number;
  let closedCount: number;
  try {
    [openCount, closedCount] = await Promise.all([
      searchIssueCount(token, REPO, "open"),
      searchIssueCount(token, REPO, "closed"),
    ]);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { notify: { body: `GitHub API 取得失敗: ${msg}` } };
  }

  const prompt =
    `リポジトリ ${REPO} の Issue 状況: open ${openCount} 件, closed ${closedCount} 件。` +
    `この数字を見て、開発者向けに 30 文字以内で一言コメントしてください。数字は繰り返さないでください。`;

  let comment: string;
  try {
    comment = await chamberlain.ai.complete({ prompt });
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    comment = `(AI コメント生成失敗: ${msg})`;
  }

  return {
    notify: {
      body: `Open ${openCount} / Closed ${closedCount} — ${comment.trim()}`,
    },
  };
}
