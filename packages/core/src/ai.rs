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
pub(crate) const ANTHROPIC_TIMEOUT_SECS: u64 = 90;

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
pub async fn complete(
    api_key: &str,
    model: Option<&str>,
    system: Option<&str>,
    messages: &[Message],
) -> Result<String, String> {
    match complete_with_tools(api_key, model, system, messages, &[]).await? {
        Completion::Text(text) => Ok(text),
        // 道具を渡していないので到達しない。潰さずエラーにする (黙って空文字を返すと
        // 呼び出し側が「モデルが何も言わなかった」と誤読する)。
        Completion::ToolUse { name, .. } => {
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
) -> Result<Completion, String> {
    let body = RequestBody {
        model: model.unwrap_or(DEFAULT_MODEL),
        max_tokens: DEFAULT_MAX_TOKENS,
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

    interpret(parsed.content).ok_or_else(|| "anthropic response had no content".to_string())
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

/// トリガーが渡してくる引数 (JSON) の形。フィールドはいずれも optional。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteArgs {
    prompt: String,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    model: Option<String>,
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
#[op2(async)]
#[string]
pub async fn op_chamberlain_ai_complete(
    state: Rc<RefCell<OpState>>,
    #[serde] args: CompleteArgs,
) -> Result<String, JsErrorBox> {
    // await 前に必要な情報を全部同期的に取り出しておく。
    // OpState を await 越しに保持しないための定型パターン。
    let api_key = {
        let mut state = state.borrow_mut();

        // 記録は呼び出しの「試行」に対して残す。キー未設定や API エラーで落ちた場合も
        // 呼びに行ったこと自体は事実で、抑えたいのは呼び出しの量だから。
        state
            .borrow_mut::<TriggerPermissions>()
            .record_ai_call(args.model.as_deref().unwrap_or(DEFAULT_MODEL));

        let service = state.borrow::<SecretsService>().0.clone();
        secret_store::get(&service, ANTHROPIC_API_KEY_NAME)
            .map_err(|e| JsErrorBox::generic(format!("failed to read anthropic_api_key: {e}")))?
            .ok_or_else(|| JsErrorBox::generic("anthropic_api_key is not set"))?
    };

    let messages = vec![Message {
        role: Role::User,
        content: args.prompt,
    }];

    complete(
        &api_key,
        args.model.as_deref(),
        args.system.as_deref(),
        &messages,
    )
    .await
    .map_err(JsErrorBox::generic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(json: &str) -> Vec<ContentBlock> {
        serde_json::from_str::<ResponseBody>(json).unwrap().content
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
