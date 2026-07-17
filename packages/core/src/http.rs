//! `chamberlain.http.fetch` — トリガーから使う汎用 HTTP クライアント op。
//!
//! - rustyscript の JS runtime には Web `fetch` が入っていないので、Rust 側で受ける
//! - 実 HTTP は reqwest (rustls) が担う (ai.rs と同じスタック)
//! - シグネチャは Web fetch を意識した最小形: `fetch(url, { method, headers, body })`
//!   → `{ status, body }` を返す。body は生テキスト、JSON パースは JS 側で行う

use std::collections::HashMap;
use std::time::Duration;

use deno_core::op2;
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};

const DEFAULT_METHOD: &str = "GET";
const DEFAULT_TIMEOUT_SECS: u64 = 30;

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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .build()
        .map_err(|e| JsErrorBox::generic(format!("http client build failed: {e}")))?;

    let mut req = client.request(method, &url);
    if let Some(headers) = opts.headers {
        for (k, v) in headers {
            req = req.header(k, v);
        }
    }
    if let Some(body) = opts.body {
        req = req.body(body);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| JsErrorBox::generic(format!("http request failed: {e}")))?;

    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| JsErrorBox::generic(format!("failed to read response body: {e}")))?;

    Ok(FetchResponse { status, body })
}
