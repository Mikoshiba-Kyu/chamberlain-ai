---
name: chamberlain-triggers
description: Chamberlain の秘書アプリで動くトリガー (定期実行タスク) を書く・直すときに読む。manifest.json の全項目、schedule DSL の使える記法と使えない記法、tick(ctx) の契約、chamberlain.* で使える API と使えないもの、動く例が入っている。「トリガーを作って」「秘書に〜させたい」と言われたときに使う。
---

# Chamberlain トリガー仕様書

**このファイル 1 つで、Chamberlain のトリガーを 1 個書けます。** ここに書いていない機能は存在しません。

**ファイルを作れる環境なら、フォルダごと作ってください** (どこに置くかは §9)。作れない環境 (チャット窓など) なら、2 つのファイルの中身をそのまま示してください。

Chamberlain は「常駐する秘書アプリ」です。アプリの中では Rust の心拍が 1 分ごとに時計を見ていて、予定の時刻が来たトリガーの `tick()` を呼びます。トリガー側が書くのは **「いつ・何を確認して・何を通知するか」** だけです。

---

## 1. 出力するもの

フォルダ 1 つ、ファイル 2 つ。これがトリガー 1 個の全体です。

```
my-trigger/
  manifest.json   # 必須 — いつ動くか、何を使ってよいか
  index.ts        # 必須 — 何をするか
```

- フォルダ名は `manifest.json` の `id` と同じにしてください。
- **`index.ts` から別ファイルを `import` することはできません。** 処理は 1 ファイルに全部書いてください (§6)。
- 出来上がったフォルダの登録方法は §9 にあります。

### 最小の完成形

`manifest.json`:

```json
{
  "id": "morning-greeting",
  "name": "朝の挨拶",
  "description": "毎朝 07:00 におはようと言う",
  "entry": "index.ts",
  "schedule": "@daily 07:00"
}
```

`index.ts`:

```typescript
interface Ctx {
  now: number;
  state: { greetCount?: number };
  scheduledAt: number;
  delayMs: number;
}

export function tick(ctx: Ctx) {
  const count = (ctx.state.greetCount ?? 0) + 1;
  return {
    notify: { body: `おはようございます (${count} 回目)` },
    state: { greetCount: count },
  };
}
```

これで「毎朝 07:00 に OS 通知が出て、回数を覚えている」トリガーになります。

---

## 2. manifest.json

| フィールド | 必須 | 内容 |
|---|---|---|
| `id` | ✅ | トリガーの識別子。フォルダ名と揃える。**ASCII 英数と `-` `_` のみ、64 文字以内**。`__meta__` / `__task__` は予約語で使えない |
| `name` | ✅ | 画面と OS 通知に出る名前 (日本語可) |
| `entry` | ✅ | エントリスクリプト。**`"index.ts"` にしてください**。フォルダの外 (`"../x.ts"` 等) を指すと実行対象から外れる |
| `schedule` | ✅ | いつ動くか。`@` 始まりの記法のみ (§3) |
| `description` | | 画面の説明文 |
| `tz` | | IANA タイムゾーン名 (`"Asia/Tokyo"` 等)。省略すると PC の設定に従う |
| `requiredSecrets` | | このトリガーが読む鍵の名前の配列 (§5.1)。**書かなければ 1 つも読めない** |
| `allowedHosts` | | このトリガーが通信してよい相手の配列 (§5.3)。**書かなければ一切ネットワークに出られない** |

- JSON にコメントは書けません。末尾カンマも不可です。
- 知らないフィールドは黙って無視されます (`version` や `author` を足しても害はありませんが、意味も持ちません)。
- `schedule` の書式が壊れている、`allowedHosts` の書き方が不正、`entry` がフォルダの外を指す — これらは **そのトリガーごと動かなくなります** (画面に「構成エラー」と出ます)。

---

## 3. schedule — いつ動くか

`schedule` は「発火する時刻の生成規則」です。時刻は `tz` (省略時は PC のローカル時刻) で解釈されます。

### 使える記法はこれで全部

<!-- spec-test: schedule-ok -->

| 記法 | 発火する時刻 |
|---|---|
| `@hourly` | 毎時 00 分 |
| `@hourly :45` | 毎時 45 分 (`:MM` は 1 つだけ) |
| `@every 5m` | 毎時 00,05,10,…,55 分 |
| `@every 10m` | 毎時 00,10,20,30,40,50 分 |
| `@every 15m` | 毎時 00,15,30,45 分 |
| `@every 20m` | 毎時 00,20,40 分 |
| `@every 30m` | 毎時 00,30 分 |
| `@daily 09:00` | 毎日 09:00 |
| `@weekly MON 09:00` | 毎週月曜 09:00 |
| `@monthly 15 09:00` | 毎月 15 日 09:00 |
| `@at 2030-08-01T18:30` | その時刻に 1 回だけ |

- **`@every` の N は 5 / 10 / 15 / 20 / 30 分のみ**です。他の値は構成エラーになります。
- 曜日は `MON` `TUE` `WED` `THU` `FRI` `SAT` `SUN` の大文字 3 文字だけです。
- 時刻は `HH:MM` (24 時間制)。`@at` は `YYYY-MM-DDTHH:MM` で、秒は書けません。**過去の日時を書いても構成エラーにはならず、ただ一度も発火しません** — 必ず未来の日時にしてください。
- `@monthly 31 09:00` のように月末に無い日を指定した月は、その月だけ発火しません。

### 使えない記法 (書かないでください)

<!-- spec-test: schedule-reject -->

| 書きたくなるもの | なぜ駄目か | 代わりに |
|---|---|---|
| `0 9 * * *` | cron 式は非対応 | `@daily 09:00` |
| `@every 7m` | 60 の約数でない | `@every 5m` / `@every 10m` |
| `@every 1h` | 単位は分だけ | `@hourly` |
| `@every 30s` | 秒は扱えない | `@every 5m` |
| `5m` | 0.2.0 で廃止された古い形式 | `@every 5m` |
| `@hourly :00,:15,:45` | カンマ区切りリストは非対応 | `@every 15m` かトリガーを分ける |
| `@daily 09:00,18:00` | 同上 | トリガーを 2 つ作る |
| `@yearly 01-01 09:00` | そんな記法は無い | `@monthly` かトリガー内で判定 |

**1 日に複数回、等間隔でない時刻に動かしたいときは、トリガーを複数作ってください。** これは制限であって、回避策のある不便ではありません。

### 心拍と遅れ

- 心拍は 1 分ごとなので、発火は最大 1 分ほど遅れます。秒単位の精度は出せません。
- PC が寝ていた等で **2 分以上遅れた定期実行は、実行されずに破棄されます** (「今さら朝 9 時の挨拶をしても不自然」という判断)。次の予定から再開します。
- 「アプリを開いていない時間帯」に予定を置くと一度も動きません。ユーザーが PC を触っている時間帯を選んでください。

---

## 4. index.ts — 何をするか

### 契約

```typescript
interface Ctx {
  /** 実行時刻 (ミリ秒 epoch) */
  now: number;
  /** 前回 tick が返した state。まだ無ければ {} */
  state: State;
  /** この実行が予定されていた時刻 (ミリ秒 epoch) */
  scheduledAt: number;
  /** now - scheduledAt。遅れをどう扱うかはトリガーが決める */
  delayMs: number;
}

interface TickResult {
  /** OS 通知を出す。title を省略すると manifest の name が使われる */
  notify?: { title?: string; body: string };
  /** 次回に持ち越す state。返した値で丸ごと置き換わる */
  state?: State;
}

export function tick(ctx: Ctx): TickResult | null;
// async function tick(ctx: Ctx): Promise<TickResult | null> でも良い
```

- **`tick` という名前の named export でなければ呼ばれません。** `export default` は認識されません。
- `async` にしてよく、返した Promise は待たれます。`chamberlain.*` を使うなら `async` が必要です。
- 戻り値のパターン:
  - `null` / 何も返さない / `{}` — 静かに終わる (通知も保存もしない)
  - `{ notify }` — 通知を出す
  - `{ state }` — state を保存する
  - `{ notify, state }` — 両方
- `notify` を返すときは **`body` が必須**です。`{ notify: {} }` は実行エラーになります。
- 例外を投げると、その回は活動ログに `[error]` として残ります。他のトリガーは影響を受けません。予定は消えるので、同じ実行が再試行されることはありません。

### state の扱い

- state は **丸ごと置き換え**です。一部だけ更新したいときは自分で展開してください: `{ state: { ...ctx.state, lastRun: ctx.now } }`
- **素の JSON 相当の値しか保存できません** (数値 / 文字列 / 真偽値 / 配列 / プレーンオブジェクト / null)。
  - `Date` は `{}` になります。**時刻はミリ秒の数値で持ってください。**
  - `NaN` / `Infinity` は `null` に、`undefined` はキーごと消え、`Set` / `Map` / 関数は `{}` になります。
- state はトリガーごとに分かれていて、他のトリガーの state は見えません。

### 通知の書き方

- 本文はトリガー側の言葉です。framework は前置きを足しません。
- 遅れて実行されたことを伝えたいなら `ctx.delayMs` を見て自分で文章を変えてください。
- 「変化が無いときは黙る」のが秘書として自然です。毎回通知するトリガーは、すぐ無視されるようになります。

---

## 5. chamberlain.* — 使える API

副作用のある操作は、すべて ambient global の `chamberlain` から呼びます。**すべて Promise を返すので `await` してください。**

TypeScript の型は自分で宣言してください (`declare const chamberlain: {...}`)。実物は実行時に framework が注入します。

```typescript
declare const chamberlain: {
  getSecret(name: string): Promise<string | null>;
  ai: {
    complete(opts: {
      prompt: string;
      system?: string;
      model?: string;
      maxTokens?: number;
    }): Promise<string>;
  };
  http: {
    fetch(
      url: string,
      opts?: {
        method?: string;
        headers?: Record<string, string>;
        body?: string;
      },
    ): Promise<{ status: number; body: string }>;
  };
};
```

### 5.1 `getSecret` — 鍵を読む

```typescript
const token = await chamberlain.getSecret("github_token");
if (!token) {
  return { notify: { body: "github_token が未設定です。設定画面から登録してください。" } };
}
```

- **`manifest.json` の `requiredSecrets` に書いた名前しか返りません。** 宣言していない名前は `null` が返り、活動ログに `[denied]` が残ります。
- 未設定のときも `null` です。**必ず null チェックを書いてください。**
- 名前は英小文字と `_` で付けてください (`github_token` / `slack_webhook_url` 等)。値はユーザーが設定画面から入れます。
- **`anthropic_api_key` は宣言しても返りません。** framework が持つ鍵です。AI を使いたいなら `ai.complete` を呼んでください。

### 5.2 `ai.complete` — AI に文章を作らせる

```typescript
const comment = await chamberlain.ai.complete({
  prompt: `未読が ${count} 件あります。30 文字以内で一言ください。`,
  system: "あなたは簡潔な秘書です。",
});
```

- 鍵の設定は不要です (framework の鍵を使います)。`model` を省略すると `claude-sonnet-5` です。
- 返るのはテキストだけです。**JSON を返させたいなら prompt でそう指示し、`JSON.parse` の失敗に備えてください。**
- 応答は既定で最大 4096 トークンです。**上限に達して途中で切れた場合は例外になります** — 切れたテキストが返ることはありません。長い応答が要るなら `maxTokens` を渡してください (1〜6144)。範囲外の値も例外です。
- 90 秒でタイムアウトします。上限が 6144 なのはこのためで、**それ以上は指定できても待てません**。切り捨てもタイムアウトも例外なので、`try` / `catch` で包んでください。
- 呼び出しは毎回活動ログに `[ai]` として残ります (model・回数・消費したトークン数だけ。prompt の中身は残りません)。切れた場合は別の行で数えられます。

```typescript
const summary = await chamberlain.ai.complete({
  prompt: `次の記事を 400 字でまとめてください。\n\n${article}`,
  maxTokens: 6144,
});
```

### 5.3 `http.fetch` — 外部と通信する

```typescript
const resp = await chamberlain.http.fetch("https://api.github.com/repos/foo/bar", {
  method: "GET",
  headers: { Authorization: `Bearer ${token}`, "User-Agent": "chamberlain-trigger" },
});
if (resp.status !== 200) {
  return { notify: { body: `取得に失敗しました (${resp.status})` } };
}
const data = JSON.parse(resp.body);
```

- **`manifest.json` の `allowedHosts` に書いたホストにしか出られません。** 宣言外は例外になり `[denied]` が残ります。
  - `"api.github.com"` — 完全一致
  - `"*.example.com"` — サブドメインのみ (`example.com` 自身は**含みません**。両方使うなら 2 つ書く)
  - 単独の `*`、`*.com`、`https://` やパス・ポートを含む書き方は**構成エラー**です (トリガーごと動かなくなります)
- **https のみ**です (平文は `localhost` と `127.0.0.0/8` だけ)。
- 返るのは `{ status, body }` だけです。**レスポンスヘッダは取れません。** `body` は文字列なので、JSON は自分で `JSON.parse` します。
- リダイレクトは 5 ホップまで追いますが、**転送先も `allowedHosts` に入っていなければ拒否されます**。
- リダイレクト込みで 30 秒、レスポンス本文は 10 MB が上限です。
- **`status` は 4xx / 5xx でも例外になりません。** 自分で確認してください。

---

## 6. できないこと

**ここが一番間違えられます。** Chamberlain のトリガーはブラウザでも Node.js でも Deno でもありません。ファイルもネットワークもプロセスも、環境として存在しません。

| 書きたくなるもの | 結果 | 代わりに |
|---|---|---|
| `fetch(url)` | **`fetch` は存在しません** (`undefined`) | `chamberlain.http.fetch(url)` |
| `import fs from "node:fs"` | モジュールを解決できず起動に失敗 | ファイルは扱えません。state を使う |
| `require(...)` / `process.env` | 存在しません | `chamberlain.getSecret` |
| `import { x } from "./helper.ts"` | **相対 import は解決できません** | 1 ファイルに全部書く |
| `import ... from "npm:..."` / URL import | 外部モジュールは読めません | 標準機能だけで書く |
| `new TextEncoder()` / `TextDecoder` | 存在しません | 文字列のまま扱う / `btoa` `atob` |
| `structuredClone(x)` | 存在しません | `JSON.parse(JSON.stringify(x))` |
| `AbortController` | 存在しません | タイムアウトは framework 側が持っています |
| `localStorage` / `window` / `document` | 存在しません (UI はありません) | state を使う |

使えるのは **素の JavaScript (ES2023 相当)** と以下です:

`console.log` / `console.error` (開発時の stderr に出るだけで、活動ログには残りません) · `JSON` · `Math` · `Date` · `Intl` · `Promise` · `crypto` (`randomUUID` / `getRandomValues` 等) · `URL` / `URLSearchParams` · `atob` / `btoa` · `setTimeout` / `setInterval`

`Deno` というグローバルも見えますが、**framework の内部用です。使わないでください。** ファイルもネットワークも入っていないので実用的なことはできず、§5 の制限 (どの鍵を読めるか / どこへ出られるか) は Rust 側で強制されているので迂回もできません。

### 実行時間の制限

- **1 回の実行は 110 秒まで**です。超えると中断され `[error]` が残ります。
- すべてのトリガーは 1 本のスレッドで順番に実行されます。**長く待つトリガーは他のトリガーを待たせます。** `setTimeout` で長時間眠るのは避けてください (「10 分後にもう一度見る」は、そういう `schedule` を書くか state に記録して次回判断します)。

---

## 7. 例

### 7.1 state を使う — 前回との差分だけ通知する

`manifest.json`:

```json
{
  "id": "disk-space-watch",
  "name": "残量ウォッチ",
  "description": "残量が前回より 10% 以上減っていたときだけ知らせる",
  "entry": "index.ts",
  "schedule": "@hourly"
}
```

`index.ts`:

```typescript
interface Ctx {
  now: number;
  state: { lastPercent?: number; lastNotifiedAt?: number };
  scheduledAt: number;
  delayMs: number;
}

/** 実際の測定の代わり。ここを本物の取得処理に差し替える。 */
function currentPercent(): number {
  return Math.round(Math.random() * 100);
}

export function tick(ctx: Ctx) {
  const percent = currentPercent();
  const previous = ctx.state.lastPercent;

  // 変化が無ければ黙る。state だけ更新して通知は返さない。
  if (previous === undefined || previous - percent < 10) {
    return { state: { ...ctx.state, lastPercent: percent } };
  }

  return {
    notify: { body: `残量が ${previous}% → ${percent}% に減りました` },
    state: { lastPercent: percent, lastNotifiedAt: ctx.now },
  };
}
```

### 7.2 secret と http を使う — GitHub の open Issue 数

`manifest.json`:

```json
{
  "id": "github-open-issues",
  "name": "GitHub の未対応 Issue",
  "description": "毎朝 09:00 に open Issue 数を知らせる",
  "entry": "index.ts",
  "schedule": "@daily 09:00",
  "requiredSecrets": ["github_token"],
  "allowedHosts": ["api.github.com"]
}
```

`index.ts`:

```typescript
declare const chamberlain: {
  getSecret(name: string): Promise<string | null>;
  http: {
    fetch(
      url: string,
      opts?: { method?: string; headers?: Record<string, string>; body?: string },
    ): Promise<{ status: number; body: string }>;
  };
};

interface Ctx {
  now: number;
  state: { lastCount?: number };
  scheduledAt: number;
  delayMs: number;
}

const REPO = "Mikoshiba-Kyu/chamberlain-ai";

export async function tick(ctx: Ctx) {
  const token = await chamberlain.getSecret("github_token");
  if (!token) {
    return { notify: { body: "github_token が未設定です。設定画面から登録してください。" } };
  }

  const q = encodeURIComponent(`repo:${REPO} is:issue is:open`);
  let count: number;
  try {
    const resp = await chamberlain.http.fetch(
      `https://api.github.com/search/issues?q=${q}&per_page=1`,
      {
        headers: {
          Authorization: `Bearer ${token}`,
          Accept: "application/vnd.github+json",
          "User-Agent": "chamberlain-trigger",
        },
      },
    );
    if (resp.status !== 200) {
      return { notify: { body: `GitHub API エラー (${resp.status})` } };
    }
    count = JSON.parse(resp.body).total_count as number;
  } catch (e) {
    return { notify: { body: `取得に失敗しました: ${e instanceof Error ? e.message : String(e)}` } };
  }

  const diff = ctx.state.lastCount === undefined ? "" : ` (前回 ${ctx.state.lastCount} 件)`;
  return {
    notify: { body: `open Issue は ${count} 件です${diff}` },
    state: { lastCount: count },
  };
}
```

### 7.3 AI を使う — 溜まったメモを要約する

`manifest.json`:

```json
{
  "id": "weekly-review",
  "name": "週次のふりかえり",
  "description": "毎週金曜 17:00 に、その週に書き溜めたメモを AI に要約させる",
  "entry": "index.ts",
  "schedule": "@weekly FRI 17:00",
  "tz": "Asia/Tokyo"
}
```

`index.ts`:

```typescript
declare const chamberlain: {
  ai: { complete(opts: { prompt: string; system?: string; model?: string }): Promise<string> };
};

interface Ctx {
  now: number;
  state: { notes?: string[] };
  scheduledAt: number;
  delayMs: number;
}

export async function tick(ctx: Ctx) {
  const notes = ctx.state.notes ?? [];
  if (notes.length === 0) {
    return { notify: { body: "今週のメモはありませんでした。" } };
  }

  let summary: string;
  try {
    summary = await chamberlain.ai.complete({
      system: "あなたは簡潔な秘書です。箇条書き 3 点以内でまとめます。",
      prompt: `今週のメモです。要点だけまとめてください。\n\n${notes.join("\n")}`,
    });
  } catch (e) {
    return { notify: { body: `要約に失敗しました: ${e instanceof Error ? e.message : String(e)}` } };
  }

  // 週が変わるのでメモは空にする。
  return {
    notify: { title: "今週のふりかえり", body: summary.trim() },
    state: { notes: [] },
  };
}
```

---

## 8. 提出前のチェックリスト

- [ ] フォルダ名 = `manifest.json` の `id` (ASCII 英数と `-` `_` だけ)
- [ ] `manifest.json` に `id` / `name` / `entry` / `schedule` が揃っている。`entry` は `"index.ts"`
- [ ] `schedule` は §3 の表にある記法そのまま。`@every` は 5/10/15/20/30 分のいずれか
- [ ] `index.ts` が `export function tick` (または `export async function tick`) を持っている
- [ ] `fetch(` を直に呼んでいない (`chamberlain.http.fetch` を使っている)
- [ ] `import` 文が 1 つも無い
- [ ] 通信するなら `allowedHosts` に、鍵を読むなら `requiredSecrets` に**宣言してある**
- [ ] `getSecret` の戻りに null チェックがある
- [ ] `http.fetch` / `ai.complete` を `try` / `catch` で囲んである
- [ ] state に `Date` やクラスのインスタンスを入れていない (時刻は数値)
- [ ] 変化が無いときに黙るようになっている

---

## 9. 作ったトリガーを動かす

### エンドユーザー (配布されたアプリを使っている人)

**アプリのチャットで秘書に頼む方法もあります。**「毎朝 9 時に〜を教えて」のように繰り返しの依頼をすると、秘書がトリガーを 1 つ書いて確認画面を出すので、内容を見て [登録する] を押し、[再起動する] を押します。ファイルを自分で置く必要はありません。

フォルダを自分で用意した場合 (この仕様書を読んだ AI に作らせた場合を含む) は次の手順です。

1. `manifest.json` と `index.ts` が入ったフォルダを、どこか分かる場所に置く (AI に作らせたなら、その場所を覚えておく)
2. アプリの「トリガー」画面 → **[フォルダから追加…]** → そのフォルダを選ぶ
3. 「何を読み、どこへ出るのか」の確認画面が出るので、内容を見て [登録する]
4. `requiredSecrets` を宣言しているなら、「設定」画面でその名前の値を入れる
5. **[再起動する]** を押す — 登録したトリガーは再起動後から動きます

アプリに最初から入っているトリガーと `id` がぶつかると登録できません。その場合は `id` (とフォルダ名) を変えてください。

外したくなったら、一覧の [解除] を押します (こちらは即時に効き、溜まっていた予定も消えます)。アプリに最初から入っているトリガーは外せませんが、[停止] はできます。

うまく動かないときは「アクティビティ」画面を見てください。`[error]` (実行時の例外) / `[denied]` (宣言していない鍵やホストを使った) / `[config error]` (manifest が壊れている) が理由付きで残ります。

### エージェント開発者 (アプリを作っている人)

`triggers/<id>/` に置けばビルド時にアプリへ焼き込まれます。`pnpm tauri dev` で起動し、トリガー一覧の [今すぐ実行] で予定を待たずに試せます。
