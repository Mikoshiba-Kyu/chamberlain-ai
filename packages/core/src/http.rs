//! `chamberlain.http.fetch` — トリガーから使う汎用 HTTP クライアント op。
//!
//! - rustyscript の JS runtime には Web `fetch` が入っていないので、Rust 側で受ける
//! - 実 HTTP は reqwest (rustls) が担う (ai.rs と同じスタック)
//! - シグネチャは Web fetch を意識した最小形: `fetch(url, { method, headers, body })`
//!   → `{ status, body }` を返す。body は生テキスト、JSON パースは JS 側で行う

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use deno_core::op2;
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};

const DEFAULT_METHOD: &str = "GET";
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// レスポンス body のバイト上限 (10 MiB)。全部メモリに載せる `resp.text()` を
/// そのまま呼ぶと、返り値の大きい先を fetch した瞬間に OOM に近付く。
/// エージェント開発者が「巨大 JSON をうっかり丸ごと吸い込む」事故を防ぐ (Issue #21 #3)。
/// 10 MiB は「テキスト API 応答としては十分、画像取得等の非想定用途には不足」の閾値。
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// http op で使う共有 Client。connection pool と TLS session を再利用する。
/// build 失敗は timeout + rustls の組合せでは想定されないので許容。
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .build()
        .expect("failed to build reqwest client for chamberlain.http.fetch")
});

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct FetchOpts {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<HashMap<String, String>>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Serialize)]
struct FetchResponse {
    status: u16,
    body: String,
}

#[op2(async)]
#[serde]
pub async fn op_chamberlain_http_fetch(
    #[string] url: String,
    #[serde] opts: Option<FetchOpts>,
) -> Result<FetchResponse, JsErrorBox> {
    let opts = opts.unwrap_or_default();
    let method_str = opts
        .method
        .as_deref()
        .unwrap_or(DEFAULT_METHOD)
        .to_uppercase();
    let method = reqwest::Method::from_bytes(method_str.as_bytes())
        .map_err(|e| JsErrorBox::generic(format!("invalid method '{method_str}': {e}")))?;

    let mut req = HTTP_CLIENT.request(method, &url);
    if let Some(headers) = opts.headers {
        for (k, v) in headers {
            req = req.header(k, v);
        }
    }
    if let Some(body) = opts.body {
        req = req.body(body);
    }

    let mut resp = req
        .send()
        .await
        .map_err(|e| JsErrorBox::generic(format!("http request failed: {e}")))?;

    let status = resp.status().as_u16();

    // Content-Length が申告されているなら先に拒否 (無駄なダウンロードを避ける)。
    // chunked や content-length 未申告のケースはチャンク読みで累積上限を守る。
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_BODY_BYTES {
            return Err(JsErrorBox::generic(format!(
                "response body size {len} exceeds limit {MAX_BODY_BYTES}"
            )));
        }
    }

    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| JsErrorBox::generic(format!("failed to read response body: {e}")))?
    {
        if buf.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(JsErrorBox::generic(format!(
                "response body exceeded limit {MAX_BODY_BYTES}"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&buf).into_owned();

    Ok(FetchResponse { status, body })
}
