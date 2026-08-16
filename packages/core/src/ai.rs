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

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: &'a [Message],
    /// 空のときは送らない。`tools: []` を送っても害は無いが、道具を渡していない
    /// 呼び出し (Type I の `ai.complete`) のリクエストを変えたくない。
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Tool<'a>]>,
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
) -> Result<(String, StopReason), String> {
    match complete_with_tools(api_key, model, system, messages, &[], max_tokens).await? {
        (Completion::Text(text), stop) => Ok((text, stop)),
        // 道具を渡していないので到達しない。潰さずエラーにする (黙って空文字を返すと
        // 呼び出し側が「モデルが何も言わなかった」と誤読する)。
        (Completion::ToolUse { name, .. }, _) => {
            Err(format!("anthropic returned an unexpected tool_use: {name}"))
        }
    }
}

/// 道具を渡して 1 往復する (#61)。
///
/// **ツール結果を返して会話を続ける経路は持たない。** 秘書がトリガーの生成を申し出る
/// 用途では、道具が呼ばれた時点で core 側の仕事 (生成 → 下書き → 同意画面) に移り、
/// 会話は定型の 1 行で閉じる。往復を重ねる形にすると、失敗のたびにモデルが勝手に
/// やり直して課金だけが伸びる。
pub async fn complete_with_tools(
    api_key: &str,
    model: Option<&str>,
    system: Option<&str>,
    messages: &[Message],
    tools: &[Tool<'_>],
    max_tokens: u32,
) -> Result<(Completion, StopReason), String> {
    let body = RequestBody {
        model: model.unwrap_or(DEFAULT_MODEL),
        max_tokens,
        system,
        messages,
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
    Ok((completion, stop))
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
/// 記録するのは model と回数だけで、**prompt は残さない**。履歴はエンドユーザーの手元に
/// 平文で溜まるので、内容まで書くと観測面がそのまま漏洩面になる。
///
/// **応答が `maxTokens` で切れたら例外にする** (#68)。「切れた要約を通知した」より
/// 「要約に失敗した」の方が秘書として正しい。長い応答が要るトリガーは `maxTokens` を
/// 上げる。
#[op2(async)]
#[string]
pub async fn op_chamberlain_ai_complete(
    state: Rc<RefCell<OpState>>,
    #[serde] args: CompleteArgs,
) -> Result<String, JsErrorBox> {
    let max_tokens = checked_max_tokens(args.max_tokens)?;
    let model = args.model.as_deref().unwrap_or(DEFAULT_MODEL);

    // await 前に必要な情報を全部同期的に取り出しておく。
    // OpState を await 越しに保持しないための定型パターン。
    let api_key = {
        let mut state = state.borrow_mut();

        // 記録は呼び出しの「試行」に対して残す。キー未設定や API エラーで落ちた場合も
        // 呼びに行ったこと自体は事実で、抑えたいのは呼び出しの量だから。
        state
            .borrow_mut::<TriggerPermissions>()
            .record_ai_call(model);

        let service = state.borrow::<SecretsService>().0.clone();
        secret_store::get(&service, ANTHROPIC_API_KEY_NAME)
            .map_err(|e| JsErrorBox::generic(format!("failed to read anthropic_api_key: {e}")))?
            .ok_or_else(|| JsErrorBox::generic("anthropic_api_key is not set"))?
    };

    let messages = vec![Message {
        role: Role::User,
        content: args.prompt,
    }];

    let (text, stop) = complete(
        &api_key,
        Some(model),
        args.system.as_deref(),
        &messages,
        max_tokens,
    )
    .await
    .map_err(JsErrorBox::generic)?;

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

    fn blocks(json: &str) -> Vec<ContentBlock> {
        serde_json::from_str::<ResponseBody>(json).unwrap().content
    }

    fn stop_reason(json: &str) -> StopReason {
        let parsed: ResponseBody = serde_json::from_str(json).unwrap();
        StopReason::from_api(parsed.stop_reason.as_deref())
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

    /// 空の応答は「テキストが空文字だった」ではなく「何も返ってこなかった」。
    #[test]
    fn empty_content_has_no_completion() {
        assert!(interpret(blocks(r#"{"content":[]}"#)).is_none());
        assert!(interpret(blocks(r#"{"content":[{"type":"text","text":"  "}]}"#)).is_none());
    }
}
