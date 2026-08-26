//! Anthropic Messages API の薄いクライアント。
//!
//! - Type II (chamberlain-core が提供する秘書 chat) と Type I (トリガーが呼ぶタスク AI) の
//!   両方から共有される
//! - streaming は扱わない。tool use は **1 往復だけ**扱う (#61 — 秘書がトリガーの生成を
//!   申し出るための口。ツールの結果を返して会話を続ける経路は無い)
//! - API キーは secret store の `anthropic_api_key` から取る (呼び出し側の責務)

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Duration;

use deno_core::{op2, OpState};
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};

use crate::permissions::TriggerPermissions;
use crate::secrets::{store as secret_store, SecretsService, ANTHROPIC_API_KEY_NAME};

pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic Messages API 呼び出しの timeout。sonnet 系は通常 5–30s だが、
/// max_tokens=4096 で長い応答を吐くと 60s 近くまで伸びるので 90s を上限にする。
/// これが無いと API 側の hang で worker の tick 全体が引きずられる (Issue #21 #2)。
///
/// **JS 実行予算 (110 秒) の下限もこれである。** 上げると framework が op 自身の許した
/// 待ち時間を横から殺すことになる (`the_js_budget_sits_between_its_two_constraints`)。
pub(crate) const ANTHROPIC_TIMEOUT_SECS: u64 = 90;

/// 応答の長さと所要時間を結ぶ観測値。[`ANTHROPIC_TIMEOUT_SECS`] の根拠と同じもので、
/// end-to-end (接続 + prompt 処理 + 生成) の実測から採っている。
const OBSERVED_MAX_TOKENS: u32 = 4096;
const OBSERVED_SECS: u32 = 60;

/// トリガーが `maxTokens` で指定できる上限 (#68)。
///
/// 切り捨ては例外なので、長い応答が要るトリガーにはここを上げる逃げ道が要る。上限を
/// 置くのは、これが**エンドユーザーの課金**で回る呼び出しだから。
///
/// **値は timeout から導く。** 90 秒で吐き切れない長さを案内すると、そこを指定した
/// トリガーは切り捨て (対処が例外文に書いてある) ではなく `operation timed out` で
/// 落ちる — #68 が潰した「読めない失敗」に別の入口から戻ることになる。
/// timeout を動かせば天井も追従し、逃げ道として成立しなくなれば
/// `the_ceiling_is_reachable_within_the_timeout` が落ちる。
pub const MAX_ALLOWED_MAX_TOKENS: u32 =
    OBSERVED_MAX_TOKENS * (ANTHROPIC_TIMEOUT_SECS as u32) / OBSERVED_SECS;

/// モデルに入る入力の上限 (Claude のコンテキスト窓)。
///
/// **入力側には時間の縛りが効かない。** 出力は生成速度に律速されるので
/// [`MAX_ALLOWED_MAX_TOKENS`] が timeout から導けるが、prompt の処理は速く、
/// 1 回の呼び出しに詰め込める入力量を決めているのはコンテキスト窓の方である。
/// run 単位の予算の天井 (`crate::MAX_ALLOWED_MAX_TOKENS_PER_RUN`) がこれを使う。
pub const MAX_CONTEXT_TOKENS: u32 = 200_000;

/// Anthropic 用の reqwest Client を 1 個だけ持ち、connection pool と TLS session を
/// 使い回す (呼び出しごとに作ると TLS handshake が毎回走る)。build 失敗は初回参照時に
/// panic するが、timeout 指定と rustls-tls feature の組合せで失敗する理由が無いので許容。
static ANTHROPIC_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(ANTHROPIC_TIMEOUT_SECS))
        .build()
        .expect("failed to build reqwest client for Anthropic")
});

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// モデルに渡す道具の定義 (#61)。`input_schema` は JSON Schema そのまま。
#[derive(Serialize)]
pub struct Tool<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub input_schema: serde_json::Value,
}

/// プロンプトキャッシュの breakpoint をどこに置くか (#72)。
///
/// **公開契約には出さない。** `chamberlain.ai.complete` の口は変えないので、トリガー
/// 作者が breakpoint を書く手段は無い。Type I の prompt は作者が 1 発ずつ組むもので、
/// core からは中身も再利用の見込みも見えない。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CacheBreakpoint {
    /// 置かない。リクエストの JSON は cache_control を入れる前と 1 バイトも変わらない。
    None,
    /// `messages` の最後のブロックに置く。render 順が `tools` → `system` → `messages`
    /// なので、**これ 1 つで道具定義・system prompt・履歴の全体が対象になる。**
    ///
    /// breakpoint は前方へ最大 20 ブロック遡って前回のキャッシュを探す。会話は 1 往復
    /// あたり 2 ブロックしか伸びないので、毎ターンここに置くだけで前ターンぶんが読める。
    LastMessage,
}

/// TTL は既定 (5 分) のまま。秘書チャットは会話中の連続ターンで、たいてい間に合う。
/// 1 時間 TTL は書き込みが 2 倍で、元を取るには同じ prefix を 3 回以上使う必要がある —
/// 会話が 5 分空いたら、それは次の会話であることの方が多い。
const EPHEMERAL: CacheControl = CacheControl { kind: "ephemeral" };

#[derive(Serialize, Clone, Copy)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<WireMessage<'a>>,
    /// 空のときは送らない。`tools: []` を送っても害は無いが、道具を渡していない
    /// 呼び出し (Type I の `ai.complete`) のリクエストを変えたくない。
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Tool<'a>]>,
}

/// リクエストに載せる 1 メッセージ。[`Message`] との違いは `content` の形だけ。
#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a Role,
    content: WireContent<'a>,
}

/// `content` は文字列でもブロック配列でもよい。cache_control はブロックにしか付かない。
///
/// **breakpoint を置く呼び出しでは、置かないメッセージも含めて全部ブロックにする。**
/// 同じ 1 通が「最後だからブロック」「次のターンでは最後でないから文字列」と形を変えると、
/// prefix が前ターンと一致する保証が消える — キャッシュは prefix の一致でしか効かない。
#[derive(Serialize)]
#[serde(untagged)]
enum WireContent<'a> {
    Text(&'a str),
    Blocks([TextBlock<'a>; 1]),
}

#[derive(Serialize)]
struct TextBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

/// [`Message`] の列をリクエストの形にする。
fn wire_messages(messages: &[Message], cache: CacheBreakpoint) -> Vec<WireMessage<'_>> {
    let last = messages.len().wrapping_sub(1);
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| WireMessage {
            role: &m.role,
            content: match cache {
                CacheBreakpoint::None => WireContent::Text(&m.content),
                CacheBreakpoint::LastMessage => WireContent::Blocks([TextBlock {
                    kind: "text",
                    text: &m.content,
                    cache_control: (i == last).then_some(EPHEMERAL),
                }]),
            },
        })
        .collect()
}

/// 応答の content ブロック。
///
/// **未知の種別を握り潰す** (`Other`)。thinking など将来増えるブロックが混ざったときに
/// パース自体が落ちると、秘書チャットが丸ごと使えなくなる。読めるものだけ拾う。
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ResponseBody {
    content: Vec<ContentBlock>,
    /// `"end_turn"` / `"max_tokens"` / `"tool_use"` / `"stop_sequence"`。
    /// **欠けていても落とさない** — 未知の値と同じく「切れていない」に倒す。
    #[serde(default)]
    stop_reason: Option<String>,
    /// 消費したトークン (#71)。**欠けていても落とさない** — `stop_reason` と同じ方針。
    #[serde(default)]
    usage: Usage,
}

/// 1 回の呼び出しが消費したトークン (#71)。
///
/// **欠損にも未知フィールドにも強くする。** `usage` ごと無い応答も、知らない項目が
/// 増えた応答も 0 として読む。これは観測のためのフィールドで、API の形が変わった日に
/// 呼び出し自体が落ちるのは本末転倒 ([`ResponseBody::stop_reason`] と同じ方針)。
///
/// **`null` も 0 として読む。** Messages API はトークン数を nullable (`integer | null`)
/// として返しうる (公式 SDK の型がそうなっている)。`#[serde(default)]` が効くのは
/// **項目ごと無いとき**だけで、明示的な `null` は `invalid type: null, expected u32`
/// になる。ここが落ちると `ResponseBody` のパースごと失敗する — 観測のために足した
/// フィールドが AI 呼び出し自体を落とすことになる。
///
/// キャッシュの 2 つを最初から持つのは、**プロンプトキャッシュが効いているかどうかを
/// 見る手段がこれしか無い**ため。最小キャッシュ長に満たない prefix はエラーにならず
/// 黙って無視されるので、`cache_read_input_tokens` が 0 のままかどうかでしか
/// 「入れたつもりで全ミス」を検出できない。
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Usage {
    #[serde(deserialize_with = "null_as_zero")]
    pub input_tokens: u32,
    #[serde(deserialize_with = "null_as_zero")]
    pub output_tokens: u32,
    #[serde(deserialize_with = "null_as_zero")]
    pub cache_creation_input_tokens: u32,
    #[serde(deserialize_with = "null_as_zero")]
    pub cache_read_input_tokens: u32,
}

/// `null` を 0 として読む。[`Usage`] の doc にある通り、API はトークン数を nullable で
/// 返しうるので、項目の欠損だけを見る `#[serde(default)]` では足りない。
fn null_as_zero<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    Ok(Option::<u32>::deserialize(d)?.unwrap_or(0))
}

impl Usage {
    /// 記録は 1 実行につき 1 行に畳まれる (`permissions::TriggerPermissions::record`) ので、
    /// 畳んだぶんを足し込めるようにする。
    ///
    /// **飽和加算。** 課金の観測のために panic する理由が無い (u32 が溢れるほどの
    /// 消費が 1 実行で起きたなら、それ自体は行の数字を見れば分かる)。
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .saturating_add(other.cache_creation_input_tokens),
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .saturating_add(other.cache_read_input_tokens),
        }
    }

    /// 何も消費していない。応答が返る前に落ちた呼び出し (キー未設定 / API エラー) が
    /// これになる。
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// 1 回の呼び出しが消費したトークンの総数 (#82)。run 単位の予算がこれを積む。
    ///
    /// **4 項目を全部足す。** キャッシュ読みは単価が安いが 0 ではなく、予算は金額では
    /// なくトークンで持つと決めてある (#71 の「換算しない」の帰結) 以上、項目ごとに
    /// 重みを付ける根拠がこの層には無い。
    ///
    /// `u64` で返すのは、run の合計が呼び出し回数だけ積み上がるため。1 回分は u32 に
    /// 収まるが、合計は収まる保証が無い。
    pub fn total(&self) -> u64 {
        u64::from(self.input_tokens)
            + u64::from(self.output_tokens)
            + u64::from(self.cache_creation_input_tokens)
            + u64::from(self.cache_read_input_tokens)
    }

    /// 活動ログの本文に消費を添える。0 のときは何も足さない — **添えない行は
    /// 「応答が返らなかった」を意味する。** `tokens in=0 out=0` と書くと、
    /// 呼びに行けなかったことと 0 トークンで返ってきたことが同じ見た目になる。
    ///
    /// キャッシュの 2 つは 0 なら書かない。プロンプトキャッシュを入れるまでは常に 0 で、
    /// 毎行 2 項目のノイズになる。入れた後は 0 でなくなるので、効いているかがそのまま
    /// 行に出る。
    pub fn annotate(&self, message: String) -> String {
        if self.is_zero() {
            return message;
        }
        let mut out = format!(
            "{message} tokens in={} out={}",
            self.input_tokens, self.output_tokens
        );
        if self.cache_creation_input_tokens > 0 {
            out.push_str(&format!(
                " cache_write={}",
                self.cache_creation_input_tokens
            ));
        }
        if self.cache_read_input_tokens > 0 {
            out.push_str(&format!(" cache_read={}", self.cache_read_input_tokens));
        }
        out
    }
}

/// 応答が終わった理由のうち、呼び出し側の判断が変わるものだけを持つ (#68)。
///
/// **`bool` ではなく型にしてある。** `Result` の `Ok` 側に混ぜて返すので、呼び出し側は
/// 必ず束縛することになる。`truncated: bool` をフィールドで足す形は見落とせる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// モデルが自分で書き終えた。
    Complete,
    /// `max_tokens` に当たって**途中で切れている**。
    Truncated,
}

impl StopReason {
    fn from_api(raw: Option<&str>) -> Self {
        match raw {
            Some("max_tokens") => Self::Truncated,
            _ => Self::Complete,
        }
    }
}

/// 1 往復の結果。`content` の型は呼ぶ関数で決まる ([`complete`] なら [`String`]、
/// [`complete_with_tools`] なら [`Completion`])。
///
/// **タプルではなく構造体にしてある。** #68 で `(結果, StopReason)` にしたところに
/// `usage` (#71) が加わって 3 要素になり、位置で読む形は誤りに気づけない。
///
/// `usage` を `Option` にしないのは、**取れなかったことと 0 だったことを区別しない**
/// ため。API が `usage` を返さない応答は起きない想定で、万一そうなっても記録が
/// 「消費 0」として出る方が、そこだけ別の分岐を書かせるより安い。
pub struct Response<T> {
    pub content: T,
    pub stop: StopReason,
    pub usage: Usage,
}

/// 1 往復の結果。道具を渡していなければ必ず [`Completion::Text`]。
pub enum Completion {
    Text(String),
    /// モデルが道具を呼びたがっている。`text` は呼ぶ前に添えてきた前置き
    /// (「承知しました、〜を作ります」)。無いこともある。
    ToolUse {
        text: Option<String>,
        name: String,
        input: serde_json::Value,
    },
}

/// Anthropic Messages API に POST し、assistant のテキスト応答を返す。
///
/// - `model` が `None` の場合は `DEFAULT_MODEL` を使う
/// - `system` は文字列そのままシステムプロンプトとして渡る
/// - `messages` は user / assistant の交互ログ (呼び出し側で組み立てる)
/// - `max_tokens` は呼び出し側が決める ([`DEFAULT_MAX_TOKENS`] を使うなら明示して渡す)
///
/// **切り捨てはここでは判断しない** (#68)。テキストと [`StopReason`] を並べて返し、
/// 「切れたものをどう扱うか」は呼び出し側が決める — Type I は例外、生成は言い直し、と
/// 経路ごとに正解が違う。
pub async fn complete(
    api_key: &str,
    model: Option<&str>,
    system: Option<&str>,
    messages: &[Message],
    max_tokens: u32,
) -> Result<Response<String>, String> {
    // 道具も履歴も持たない 1 発の呼び出しなので、キャッシュする prefix が無い (#72)。
    let response = complete_with_tools(
        api_key,
        model,
        system,
        messages,
        &[],
        max_tokens,
        CacheBreakpoint::None,
    )
    .await?;
    match response.content {
        Completion::Text(text) => Ok(Response {
            content: text,
            stop: response.stop,
            usage: response.usage,
        }),
        // 道具を渡していないので到達しない。潰さずエラーにする (黙って空文字を返すと
        // 呼び出し側が「モデルが何も言わなかった」と誤読する)。
        Completion::ToolUse { name, .. } => {
            Err(format!("anthropic returned an unexpected tool_use: {name}"))
        }
    }
}

/// 呼び出しに実際に使うモデル名。**送る値と記録に載る値を同じ関数から取る** (#71) —
/// 既定を使った呼び出しが活動ログに `model=` 無しで並ぶと、消費をモデル別に読めない。
pub fn resolve_model(model: Option<&str>) -> &str {
    model.unwrap_or(DEFAULT_MODEL)
}

/// 道具を渡して 1 往復する (#61)。
///
/// **ツール結果を返して会話を続ける経路は持たない。** 秘書がトリガーの生成を申し出る
/// 用途では、道具が呼ばれた時点で core 側の仕事 (生成 → 下書き → 同意画面) に移り、
/// 会話は定型の 1 行で閉じる。往復を重ねる形にすると、失敗のたびにモデルが勝手に
/// やり直して課金だけが伸びる。
///
/// `cache` はプロンプトキャッシュの breakpoint (#72)。**呼び出し側が決める** — 効くのは
/// 同じ prefix を短い間隔で何度も送る経路だけで、それを知っているのはここではない。
pub async fn complete_with_tools(
    api_key: &str,
    model: Option<&str>,
    system: Option<&str>,
    messages: &[Message],
    tools: &[Tool<'_>],
    max_tokens: u32,
    cache: CacheBreakpoint,
) -> Result<Response<Completion>, String> {
    let body = RequestBody {
        model: resolve_model(model),
        max_tokens,
        system,
        messages: wire_messages(messages, cache),
        tools: (!tools.is_empty()).then_some(tools),
    };

    let response = ANTHROPIC_CLIENT
        .post(ANTHROPIC_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("http request failed: {e}"))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("failed to read response body: {e}"))?;

    if !status.is_success() {
        return Err(format!("anthropic API error {status}: {text}"));
    }

    let parsed: ResponseBody = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse anthropic response: {e} — body: {text}"))?;

    let stop = StopReason::from_api(parsed.stop_reason.as_deref());
    let completion = match interpret(parsed.content) {
        Some(completion) => completion,
        // 読めるものが 1 つも無いまま `max_tokens` に当たった (`maxTokens: 1` で空白 1
        // トークンだけ、等)。ここで「応答が無かった」に倒すと**切り捨てが別の失敗に
        // 化ける** — 呼び出し側の分岐 (#68) を素通りし、`[ai]` の切り捨て行も残らない。
        // 空のテキストとして渡し、扱いは他の切り捨てと同じ経路に乗せる。
        None if stop == StopReason::Truncated => Completion::Text(String::new()),
        None => return Err("anthropic response had no content".to_string()),
    };
    Ok(Response {
        content: completion,
        stop,
        usage: parsed.usage,
    })
}

/// content ブロックの列を 1 つの結果に畳む。
///
/// ツール呼び出しがあればそちらが主で、テキストは前置きとして添える。**順序に頼らない** —
/// text と tool_use のどちらが先に来るかは応答ごとに違う。
fn interpret(blocks: Vec<ContentBlock>) -> Option<Completion> {
    let mut texts: Vec<String> = Vec::new();
    let mut tool: Option<(String, serde_json::Value)> = None;

    for block in blocks {
        match block {
            ContentBlock::Text { text } => texts.push(text),
            // 複数呼ばれても最初の 1 つだけ見る。core が受けられる道具は 1 つで、
            // 2 つ目を黙って捨てる方が「両方やったつもり」になるより読みやすい。
            ContentBlock::ToolUse { name, input } if tool.is_none() => tool = Some((name, input)),
            _ => {}
        }
    }

    let text = {
        let joined = texts.join("\n").trim().to_string();
        (!joined.is_empty()).then_some(joined)
    };

    match tool {
        Some((name, input)) => Some(Completion::ToolUse { text, name, input }),
        None => text.map(Completion::Text),
    }
}

// --- deno_core op (JS runtime から `chamberlain.ai.complete(...)` として呼ばれる) ---

/// トリガーが渡してくる引数 (JSON)。`prompt` 以外は optional。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteArgs {
    prompt: String,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// 応答の長さの上限 (#68)。既定は [`DEFAULT_MAX_TOKENS`]、上限は
    /// [`MAX_ALLOWED_MAX_TOKENS`]。範囲外は**丸めずに例外**にする — 黙って丸めると
    /// 「上げたのに切れる」が起き、切り捨てが例外である意味が消える。
    #[serde(default)]
    max_tokens: Option<u32>,
}

/// トリガーが指定した `maxTokens` を検証して確定値にする。
///
/// **ここで既定を潰しておく。** 送る値とエラー文・記録に載る値が同じ変数から来るので、
/// 「切れたと言われた上限」と「実際に送った上限」が食い違いようがない。
fn checked_max_tokens(requested: Option<u32>) -> Result<u32, JsErrorBox> {
    match requested {
        None => Ok(DEFAULT_MAX_TOKENS),
        Some(n) if (1..=MAX_ALLOWED_MAX_TOKENS).contains(&n) => Ok(n),
        Some(n) => Err(JsErrorBox::generic(format!(
            "maxTokens must be between 1 and {MAX_ALLOWED_MAX_TOKENS} (got {n})"
        ))),
    }
}

/// トリガーから呼ばれる `chamberlain.ai.complete`。
///
/// **宛先の宣言は取らない代わりに、呼び出しを必ず履歴に残す** (#57)。宛先は core が決める
/// (Anthropic 固定) ので `allowedHosts` のような形が取れない一方、これは framework が持つ
/// API キーの持ち出しにあたる — 無制限に呼べると、エンドユーザーの課金でトリガー作者の
/// 用事が処理される。レート制限は必要になってから。
///
/// 記録するのは model・回数・消費したトークン数で、**prompt は残さない**。履歴は
/// エンドユーザーの手元に平文で溜まるので、内容まで書くと観測面がそのまま漏洩面になる。
/// **token 数は数値なのでこの線を越えない** (#71)。
///
/// **応答が `maxTokens` で切れたら例外にする** (#68)。「切れた要約を通知した」より
/// 「要約に失敗した」の方が秘書として正しい。長い応答が要るトリガーは `maxTokens` を
/// 上げる。
///
/// **1 回の実行で消費できる総量には天井がある** (#82)。`maxTokens` は 1 回あたりの
/// 上限にすぎず、回数に上限が無い状態では 1 run の消費が青天井になる。予算を使い切った
/// 後の呼び出しは例外になり、`[denied]` が残る。
#[op2(async)]
#[string]
pub async fn op_chamberlain_ai_complete(
    state: Rc<RefCell<OpState>>,
    #[serde] args: CompleteArgs,
) -> Result<String, JsErrorBox> {
    let max_tokens = checked_max_tokens(args.max_tokens)?;
    let model = resolve_model(args.model.as_deref());

    // await 前に必要な情報を全部同期的に取り出しておく。
    // OpState を await 越しに保持しないための定型パターン。
    let api_key = {
        let mut state = state.borrow_mut();

        // 予算の判定が先 (#82)。断った呼び出しは `[denied]` として残り、`[ai]` には
        // 現れない — 呼びに行っていないものを「呼び出しの量」に数えると、予算が
        // 効いているほど使ったように見える。
        //
        // 記録は呼び出しの「試行」に対して残す。キー未設定や API エラーで落ちた場合も
        // 呼びに行ったこと自体は事実で、抑えたいのは呼び出しの量だから。
        let perms = state.borrow_mut::<TriggerPermissions>();
        perms
            .authorize_ai_call(max_tokens)
            .map_err(JsErrorBox::generic)?;
        perms.record_ai_call(model);

        let service = state.borrow::<SecretsService>().0.clone();
        secret_store::get(&service, ANTHROPIC_API_KEY_NAME)
            .map_err(|e| JsErrorBox::generic(format!("failed to read anthropic_api_key: {e}")))?
            .ok_or_else(|| JsErrorBox::generic("anthropic_api_key is not set"))?
    };

    let messages = vec![Message {
        role: Role::User,
        content: args.prompt,
    }];

    let response = complete(
        &api_key,
        Some(model),
        args.system.as_deref(),
        &messages,
        max_tokens,
    )
    .await;

    // 予約 (#82) は成否に関わらずここで外す。**`?` より先に。** 落ちた呼び出しの予約を
    // 残すと、返ってこなかったぶんだけ予算が目減りしたまま実行が続く。
    state
        .borrow_mut()
        .borrow_mut::<TriggerPermissions>()
        .release_ai_reservation(max_tokens);

    let Response {
        content: text,
        stop,
        usage,
    } = response.map_err(JsErrorBox::generic)?;

    // 消費を呼び出しの行に足す (#71)。**切り捨ての分岐より先に。** 切れた応答は
    // `max_tokens` を吐き切っているので消費は最大で、そこを例外の陰で落とすと
    // 「一番高い呼び出しだけ記録に出ない」ことになる。
    state
        .borrow_mut()
        .borrow_mut::<TriggerPermissions>()
        .record_ai_usage(model, usage);

    if stop == StopReason::Truncated {
        // 例外は catch されうるので、切り捨て自体は観測面にも残す。`[ai]` の行を分けて
        // いるのは、`record` が同じ本文を畳む以上「何回中いくつが切れたか」がそこでしか
        // 数えられないため。
        state
            .borrow_mut()
            .borrow_mut::<TriggerPermissions>()
            .record_ai_truncation(model);
        return Err(JsErrorBox::generic(format!(
            "ai.complete response was truncated at maxTokens={max_tokens}; raise maxTokens (up to {MAX_ALLOWED_MAX_TOKENS}) or ask for a shorter answer"
        )));
    }

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::TriggerGrants;
    use crate::trigger_runtime_options;
    use std::collections::BTreeMap;

    /// 予算を使い切った後の `ai.complete` は **JS に例外として届き、ネットワークには
    /// 出ない** (#82)。
    ///
    /// このテストがネットワークもキーも要らないこと自体が主張である — 判定が呼び出しの
    /// 手前にあるなら、API キーを読むより先に落ちるはずで、`SecretsService` を
    /// `OpState` に置いていないのに動くことがその証拠になる。
    #[test]
    fn a_call_over_the_budget_is_rejected_before_anything_is_sent() {
        // 本番の worker と同じ構成で立てる (`trigger_runtime_options`)。ここで
        // 組み直すと、拡張が変わったときに別の isolate に対して合格し続ける。
        let mut runtime = rustyscript::Runtime::new(trigger_runtime_options())
            .expect("failed to init JS runtime");
        {
            let op_state = runtime.deno_runtime().op_state();
            let mut op_state = op_state.borrow_mut();
            let grants = TriggerGrants {
                max_tokens_per_run: 100,
                ..Default::default()
            };
            let mut perms = TriggerPermissions::new(BTreeMap::from([("t".to_string(), grants)]));
            perms.enter("t");
            op_state.put(perms);
        }

        let module = rustyscript::Module::new(
            "over-budget.ts",
            r#"
            export async function tick() {
              try {
                await chamberlain.ai.complete({ prompt: "hi", maxTokens: 200 });
                return "reached the API";
              } catch (e) {
                return String(e);
              }
            }
            "#,
        );
        let handle = runtime.load_module(&module).expect("failed to load module");
        let result: String = runtime
            .call_function(Some(&handle), "tick", rustyscript::json_args!())
            .expect("tick failed");

        assert!(
            result.contains("maxTokensPerRun"),
            "予算超過が送信前に落ちていない: {result}"
        );

        let op_state = runtime.deno_runtime().op_state();
        let recorded = op_state
            .borrow_mut()
            .borrow_mut::<TriggerPermissions>()
            .leave();
        assert_eq!(
            recorded.len(),
            1,
            "拒否が観測面に残っていない: {recorded:?}"
        );
        assert_eq!(recorded[0].trigger_id.as_deref(), Some("t"));
    }

    fn convo(contents: &[&str]) -> Vec<Message> {
        contents
            .iter()
            .enumerate()
            .map(|(i, c)| Message {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: (*c).to_string(),
            })
            .collect()
    }

    fn wire(messages: &[Message], cache: CacheBreakpoint) -> serde_json::Value {
        serde_json::to_value(wire_messages(messages, cache)).unwrap()
    }

    /// breakpoint を置かない呼び出しのリクエストは、#72 の前と 1 バイトも変わらない。
    ///
    /// **Type I (`ai.complete`) と下書きの生成がここを通る。** キャッシュを入れた副作用で
    /// それらの `content` がブロック配列に変わると、公開契約に出さないと決めた変更が
    /// トリガー作者から見えるリクエストに漏れる。
    #[test]
    fn no_breakpoint_leaves_the_request_shape_untouched() {
        let messages = convo(&["ask", "answer", "again"]);
        assert_eq!(
            wire(&messages, CacheBreakpoint::None),
            serde_json::json!([
                {"role": "user", "content": "ask"},
                {"role": "assistant", "content": "answer"},
                {"role": "user", "content": "again"},
            ])
        );
    }

    /// breakpoint は末尾に 1 つだけ。**残りもブロック配列にする。**
    ///
    /// 同じ 1 通が「今は最後だからブロック」「次のターンでは最後でないから文字列」と
    /// 形を変えると、次のターンの prefix が前のターンと一致しない。キャッシュは prefix の
    /// 一致でしか効かないので、それは breakpoint を置いていないのと同じことになる。
    #[test]
    fn the_breakpoint_sits_on_the_last_block_and_the_rest_are_still_blocks() {
        let messages = convo(&["ask", "answer", "again"]);
        assert_eq!(
            wire(&messages, CacheBreakpoint::LastMessage),
            serde_json::json!([
                {"role": "user", "content": [{"type": "text", "text": "ask"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "answer"}]},
                {"role": "user", "content": [
                    {"type": "text", "text": "again", "cache_control": {"type": "ephemeral"}}
                ]},
            ])
        );
    }

    /// 会話が伸びても、前のターンで送ったぶんの bytes は変わらない。
    ///
    /// これが崩れると `cache_read_input_tokens` は 0 のまま、しかも**エラーにはならない** —
    /// 最小キャッシュ長に届かなかった場合と見分けがつかなくなる。
    #[test]
    fn growing_the_conversation_keeps_the_earlier_prefix_byte_identical() {
        let turn_n = convo(&["ask", "answer", "again"]);
        let turn_next = convo(&["ask", "answer", "again", "sure", "more"]);

        let before = wire(&turn_n, CacheBreakpoint::LastMessage);
        let after = wire(&turn_next, CacheBreakpoint::LastMessage);
        let (before, after) = (before.as_array().unwrap(), after.as_array().unwrap());

        // 末尾 (breakpoint が乗っていた 1 通) 以外はそのまま。
        assert_eq!(before[..before.len() - 1], after[..before.len() - 1]);
        // 末尾からは cache_control が外れ、それ以外は変わらない。
        assert_eq!(
            after[before.len() - 1],
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "again"}]})
        );
    }

    /// 空の messages で panic しない (`len() - 1` を素で書くと 0 で落ちる)。
    #[test]
    fn an_empty_conversation_is_not_a_panic() {
        assert_eq!(
            wire(&[], CacheBreakpoint::LastMessage),
            serde_json::json!([])
        );
    }

    fn blocks(json: &str) -> Vec<ContentBlock> {
        serde_json::from_str::<ResponseBody>(json).unwrap().content
    }

    fn stop_reason(json: &str) -> StopReason {
        let parsed: ResponseBody = serde_json::from_str(json).unwrap();
        StopReason::from_api(parsed.stop_reason.as_deref())
    }

    fn usage(json: &str) -> Usage {
        serde_json::from_str::<ResponseBody>(json).unwrap().usage
    }

    /// 天井が timeout の中で届く値であること (#68)。
    ///
    /// 届かない値を案内すると、そこを指定したトリガーは切り捨て (例外文に対処が書いて
    /// ある) ではなく `operation timed out` で落ちる。既定を上回っていることも一緒に
    /// 見る — 下回れば逃げ道として成立していない。
    #[test]
    fn the_ceiling_is_reachable_within_the_timeout() {
        let secs = MAX_ALLOWED_MAX_TOKENS * OBSERVED_SECS / OBSERVED_MAX_TOKENS;
        assert!(
            u64::from(secs) <= ANTHROPIC_TIMEOUT_SECS,
            "上限まで使うと {secs}s かかる: timeout ({ANTHROPIC_TIMEOUT_SECS}s) に届かない"
        );
        const {
            assert!(
                MAX_ALLOWED_MAX_TOKENS > DEFAULT_MAX_TOKENS,
                "天井が既定以下: maxTokens が逃げ道になっていない"
            )
        };
    }

    /// 切り捨てだけを切り捨てとして読む (#68)。ここが緩むと、正常に書き終えた応答まで
    /// Type I の例外に化ける。
    #[test]
    fn only_max_tokens_counts_as_truncation() {
        assert_eq!(
            stop_reason(r#"{"content":[],"stop_reason":"max_tokens"}"#),
            StopReason::Truncated
        );
        for body in [
            r#"{"content":[],"stop_reason":"end_turn"}"#,
            r#"{"content":[],"stop_reason":"tool_use"}"#,
            // 知らない stop_reason と、そもそも欠けている応答。**落とさず「切れていない」に
            // 倒す。** ここで panic すると、増えた値ひとつでチャットが丸ごと使えなくなる。
            r#"{"content":[],"stop_reason":"pause_turn"}"#,
            r#"{"content":[],"stop_reason":null}"#,
            r#"{"content":[]}"#,
        ] {
            assert_eq!(stop_reason(body), StopReason::Complete, "{body}");
        }
    }

    /// `maxTokens` の範囲外は**丸めずに例外**。黙って丸めると「上げたのに切れる」が起き、
    /// 切り捨てが例外である意味が消える。
    #[test]
    fn max_tokens_outside_the_allowed_range_is_rejected() {
        assert_eq!(checked_max_tokens(None).unwrap(), DEFAULT_MAX_TOKENS);
        assert_eq!(checked_max_tokens(Some(1)).unwrap(), 1);
        assert_eq!(
            checked_max_tokens(Some(MAX_ALLOWED_MAX_TOKENS)).unwrap(),
            MAX_ALLOWED_MAX_TOKENS
        );
        for out_of_range in [0, MAX_ALLOWED_MAX_TOKENS + 1] {
            assert!(
                checked_max_tokens(Some(out_of_range)).is_err(),
                "maxTokens={out_of_range} は断るはず"
            );
        }
    }

    /// 知らないブロック (thinking 等) が混ざってもパースは通る。ここが落ちると
    /// 秘書チャットが丸ごと使えなくなる。
    #[test]
    fn unknown_blocks_do_not_break_parsing() {
        let parsed = interpret(blocks(
            r#"{"content":[{"type":"thinking","thinking":"…"},{"type":"text","text":"はい"}]}"#,
        ));
        assert!(matches!(parsed, Some(Completion::Text(t)) if t == "はい"));
    }

    /// tool_use があればそれが主。前置きのテキストは添えるだけで、text 扱いにはしない。
    #[test]
    fn tool_use_wins_over_the_preamble() {
        let parsed = interpret(blocks(
            r#"{"content":[
                {"type":"text","text":"承知しました。"},
                {"type":"tool_use","id":"x","name":"propose_trigger","input":{"request":"毎朝9時"}}
            ]}"#,
        ));
        let Some(Completion::ToolUse { text, name, input }) = parsed else {
            panic!("tool_use を拾えていない");
        };
        assert_eq!(text.as_deref(), Some("承知しました。"));
        assert_eq!(name, "propose_trigger");
        assert_eq!(input["request"], "毎朝9時");
    }

    /// 順序に頼らない。tool_use が先に来る応答もある。
    #[test]
    fn preamble_after_the_tool_use_is_still_picked_up() {
        let parsed = interpret(blocks(
            r#"{"content":[
                {"type":"tool_use","id":"x","name":"propose_trigger","input":{}},
                {"type":"text","text":"用意します。"}
            ]}"#,
        ));
        assert!(
            matches!(parsed, Some(Completion::ToolUse { text, .. }) if text.as_deref() == Some("用意します。"))
        );
    }

    /// usage は**欠けても未知の項目が増えても落とさない** (#71)。ここが厳しいと、
    /// 課金を観測するために足したフィールドのせいで呼び出し自体が使えなくなる。
    #[test]
    fn missing_or_unknown_usage_fields_read_as_zero() {
        assert_eq!(
            usage(r#"{"content":[],"usage":{"input_tokens":120,"output_tokens":34}}"#),
            Usage {
                input_tokens: 120,
                output_tokens: 34,
                ..Usage::default()
            }
        );
        // キャッシュを使った応答。プロンプトキャッシュ (#71 の後続) が効いているかは
        // ここが 0 でなくなるかどうかでしか見えない。
        assert_eq!(
            usage(
                r#"{"content":[],"usage":{"input_tokens":4,"output_tokens":9,
                   "cache_creation_input_tokens":1024,"cache_read_input_tokens":2048,
                   "server_tool_use":{"web_search_requests":1}}}"#
            ),
            Usage {
                input_tokens: 4,
                output_tokens: 9,
                cache_creation_input_tokens: 1024,
                cache_read_input_tokens: 2048,
            }
        );
        for body in [r#"{"content":[]}"#, r#"{"content":[],"usage":{}}"#] {
            assert!(usage(body).is_zero(), "{body}");
        }
    }

    /// **`null` も 0 として読む。** Messages API はトークン数を nullable で返しうる。
    /// `#[serde(default)]` は項目の欠損しか見ないので、ここを `u32` のまま受けると
    /// `ResponseBody` のパースごと落ち、**その AI 呼び出しが丸ごと失敗する**。
    #[test]
    fn null_token_counts_read_as_zero() {
        assert_eq!(
            usage(
                r#"{"content":[],"usage":{"input_tokens":2095,"output_tokens":503,
                   "cache_creation_input_tokens":null,"cache_read_input_tokens":null}}"#
            ),
            Usage {
                input_tokens: 2095,
                output_tokens: 503,
                ..Usage::default()
            }
        );
    }

    /// 活動ログの行の形 (#71)。
    ///
    /// **0 のときは何も添えない。** 添えない行は「応答が返らなかった」(キー未設定 /
    /// API エラー) を意味する。`tokens in=0 out=0` と書くと、呼びに行けなかったことと
    /// 0 トークンで返ってきたことが同じ見た目になる。
    #[test]
    fn the_activity_line_carries_only_the_numbers_that_mean_something() {
        let base = || "ai.complete model=claude-sonnet-5".to_string();
        assert_eq!(Usage::default().annotate(base()), base());

        let plain = Usage {
            input_tokens: 3120,
            output_tokens: 880,
            ..Usage::default()
        };
        assert_eq!(
            plain.annotate(base()),
            "ai.complete model=claude-sonnet-5 tokens in=3120 out=880"
        );

        let cached = Usage {
            cache_creation_input_tokens: 1024,
            cache_read_input_tokens: 2048,
            ..plain
        };
        assert_eq!(
            cached.annotate(base()),
            "ai.complete model=claude-sonnet-5 tokens in=3120 out=880 cache_write=1024 cache_read=2048"
        );
    }

    /// 畳まれた行のために足せること。飽和で止まり、panic しない。
    #[test]
    fn usage_adds_up_and_saturates() {
        let a = Usage {
            input_tokens: 10,
            output_tokens: 2,
            cache_creation_input_tokens: 1,
            cache_read_input_tokens: 3,
        };
        assert_eq!(
            a.saturating_add(a),
            Usage {
                input_tokens: 20,
                output_tokens: 4,
                cache_creation_input_tokens: 2,
                cache_read_input_tokens: 6,
            }
        );
        let huge = Usage {
            input_tokens: u32::MAX,
            ..Usage::default()
        };
        assert_eq!(huge.saturating_add(huge).input_tokens, u32::MAX);
    }

    /// 空の応答は「テキストが空文字だった」ではなく「何も返ってこなかった」。
    #[test]
    fn empty_content_has_no_completion() {
        assert!(interpret(blocks(r#"{"content":[]}"#)).is_none());
        assert!(interpret(blocks(r#"{"content":[{"type":"text","text":"  "}]}"#)).is_none());
    }
}
