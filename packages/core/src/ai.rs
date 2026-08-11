//! Anthropic Messages API の薄いクライアント。
//!
//! - Type II (chamberlain-core が提供する秘書 chat) と Type I (トリガーが呼ぶタスク AI) の
//!   両方から共有される
//! - MVP スコープ: chat completion のみ。streaming / tool use は扱わない
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
const ANTHROPIC_TIMEOUT_SECS: u64 = 90;

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

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: &'a [Message],
}

#[derive(Deserialize)]
struct ResponseContent {
    #[serde(rename = "type")]
    _type: String,
    text: String,
}

#[derive(Deserialize)]
struct ResponseBody {
    content: Vec<ResponseContent>,
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
    let body = RequestBody {
        model: model.unwrap_or(DEFAULT_MODEL),
        max_tokens: DEFAULT_MAX_TOKENS,
        system,
        messages,
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

    parsed
        .content
        .into_iter()
        .next()
        .map(|c| c.text)
        .ok_or_else(|| "anthropic response had no content".to_string())
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
