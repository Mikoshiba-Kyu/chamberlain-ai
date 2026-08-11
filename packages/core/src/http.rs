//! `chamberlain.http.fetch` — トリガーから使う汎用 HTTP クライアント op。
//!
//! - rustyscript の JS runtime には Web `fetch` が入っていないので、Rust 側で受ける
//! - 実 HTTP は reqwest (rustls) が担う (ai.rs と同じスタック)
//! - シグネチャは Web fetch を意識した最小形: `fetch(url, { method, headers, body })`
//!   → `{ status, body }` を返す。body は生テキスト、JSON パースは JS 側で行う
//!
//! # ここが唯一のネットワーク出口である (#57)
//!
//! rustyscript が引く deno クレートは `deno_console` / `deno_crypto` / `deno_url` /
//! `deno_webidl` の 4 つだけで、`deno_fetch` も `deno_net` も入っていない。**JS から
//! ネットワークに出る手段はこの op しか存在しない**ので、ここを締めれば漏れがない。
//! 宛先は manifest の `allowedHosts` で宣言させ、宣言外は拒否する ([`crate::permissions`])。
//!
//! これは `getSecret` のスコープ (#56) とセットで初めて意味を持つ。secret をスコープしても
//! 出口が空いていれば、正当に持つ token を任意の場所へ送れるため。両方あって初めて
//! 「GitHub token は `api.github.com` にしか出ない」が機構として言える。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Duration;

use deno_core::{op2, OpState};
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};

use crate::permissions::{self, FetchTarget, PermissionSnapshot, TriggerPermissions};

const DEFAULT_METHOD: &str = "GET";
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// **リダイレクトを含めた** 1 回の `fetch` 全体の上限。`Client::timeout` は 1 リクエスト
/// 単位なので、追跡を自前でやるとこれが無い限り 30s × (1 + MAX_REDIRECTS) まで伸びる。
const TOTAL_TIMEOUT: Duration = Duration::from_secs(DEFAULT_TIMEOUT_SECS);

/// 追跡するリダイレクトの上限。reqwest の既定 (10) より小さくしてある。ホップごとに
/// 宣言との照合が入るので、深い連鎖は「宣言が実態に合っていない」兆候と見なしてよい。
const MAX_REDIRECTS: usize = 5;

/// レスポンス body のバイト上限 (10 MiB)。全部メモリに載せる `resp.text()` を
/// そのまま呼ぶと、返り値の大きい先を fetch した瞬間に OOM に近付く。
/// エージェント開発者が「巨大 JSON をうっかり丸ごと吸い込む」事故を防ぐ (Issue #21 #3)。
/// 10 MiB は「テキスト API 応答としては十分、画像取得等の非想定用途には不足」の閾値。
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// http op で使う共有 Client。connection pool と TLS session を再利用する。
/// build 失敗は timeout + rustls の組合せでは想定されないので許容。
///
/// **リダイレクトの自動追跡は切ってある** (#57)。reqwest に任せると宣言の照合を挟めず、
/// 宣言済みホストが 302 で任意の宛先を指すだけで制限が抜ける。追跡は
/// [`op_chamberlain_http_fetch`] が 1 ホップずつ行い、その都度 `allowedHosts` と突き合わせる。
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
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

/// URL から判断に要る 2 つ (スキーム / ホスト) を取り出す。
fn parse_url(url: &str) -> Result<reqwest::Url, JsErrorBox> {
    reqwest::Url::parse(url).map_err(|e| JsErrorBox::generic(format!("invalid url '{url}': {e}")))
}

/// URL を判断層の語彙 ([`FetchTarget`]) に翻訳して照合する。副作用を持たない。
///
/// **URL の意味論を知っているのはここだけ**で、[`crate::permissions`] は `reqwest` を
/// 知らない。ポートやパスを落とし、ホストを小文字に正規化するのがこの関数の仕事。
fn check(
    snapshot: &PermissionSnapshot,
    url: &reqwest::Url,
    hop: bool,
) -> Result<(), permissions::OpActivity> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    snapshot.check_host(
        &FetchTarget {
            scheme: url.scheme(),
            host: &host,
        },
        hop,
    )
}

/// [`check`] の結果を副作用に繋ぐ。拒否なら `OpState` に記録してから `Err` を返す。
///
/// `getSecret` は `null` を返す (未設定と同じ形) が、こちらは**例外にする**。fetch に
/// 「値が無い」に相当する戻り値は無く、握り潰すとトリガーは空レスポンスを掴んで進む。
fn authorize(
    state: &Rc<RefCell<OpState>>,
    snapshot: &PermissionSnapshot,
    url: &reqwest::Url,
    hop: bool,
) -> Result<(), JsErrorBox> {
    match check(snapshot, url, hop) {
        Ok(()) => Ok(()),
        Err(denial) => {
            let message = denial.message.clone();
            state
                .borrow_mut()
                .borrow_mut::<TriggerPermissions>()
                .record(denial);
            Err(JsErrorBox::generic(message))
        }
    }
}

/// リダイレクト先を決める。追跡しないなら `None`。
///
/// メソッドの扱いはブラウザの慣習に合わせる — 303 は常に GET、301/302 は POST のみ GET に
/// 落とす、307/308 はメソッドと body を保つ。ここを外すと「リダイレクトを追う」だけで
/// 意味論が変わってしまう。
fn next_hop(
    resp: &reqwest::Response,
    current: &reqwest::Url,
    method: &reqwest::Method,
) -> Option<(reqwest::Url, reqwest::Method)> {
    let status = resp.status();
    if !status.is_redirection() {
        return None;
    }
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    let next = current.join(location).ok()?;

    let method = match status.as_u16() {
        303 => reqwest::Method::GET,
        301 | 302 if *method == reqwest::Method::POST => reqwest::Method::GET,
        _ => method.clone(),
    };
    Some((next, method))
}

/// 転送先が別ホストなら、認証情報を持つヘッダを落とす。
///
/// **reqwest の redirect policy を切った代償**。あちらは追跡時にこれを自前でやっていたので、
/// 追跡を core に移した時点で一緒に落ちていた。`allowedHosts` は守りにならない —
/// `["api.github.com", "*.githubusercontent.com"]` のように**両方とも正当に宣言されている**
/// 構成 (GitHub の asset 取得はまさにこれ) で、片方向けの token がもう片方に届いてしまう。
///
/// 落とす対象と cross-host の判定は reqwest の `remove_sensitive_headers` に合わせてある
/// (ホストに加えてポートも見る)。
fn strip_sensitive_headers_on_cross_host(
    headers: &mut HashMap<String, String>,
    current: &reqwest::Url,
    next: &reqwest::Url,
) {
    let cross_host = next.host_str() != current.host_str()
        || next.port_or_known_default() != current.port_or_known_default();
    if !cross_host {
        return;
    }
    headers.retain(|name, _| {
        !matches!(
            name.to_ascii_lowercase().as_str(),
            "authorization" | "cookie" | "cookie2" | "proxy-authorization" | "www-authenticate"
        )
    });
}

#[op2(async)]
#[serde]
pub async fn op_chamberlain_http_fetch(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[serde] opts: Option<FetchOpts>,
) -> Result<FetchResponse, JsErrorBox> {
    // await の前に実行文脈を写し取る。OpState は await を跨いで借りられないが、
    // リダイレクトのホップごとに判断が要るため (permissions モジュール doc 参照)。
    let snapshot = state.borrow().borrow::<TriggerPermissions>().snapshot();

    let opts = opts.unwrap_or_default();
    let method_str = opts
        .method
        .as_deref()
        .unwrap_or(DEFAULT_METHOD)
        .to_uppercase();
    let mut method = reqwest::Method::from_bytes(method_str.as_bytes())
        .map_err(|e| JsErrorBox::generic(format!("invalid method '{method_str}': {e}")))?;

    let mut current = parse_url(&url)?;
    authorize(&state, &snapshot, &current, false)?;

    let mut headers = opts.headers.unwrap_or_default();
    let mut body = opts.body;
    let mut hops = 0usize;
    // リダイレクト込みの締め切り。reqwest の `Client::timeout` は 1 リクエスト単位なので、
    // 追跡を自前でやると 30s × ホップ数まで伸びる。JS は単一スレッドで直列なので、
    // 1 本の fetch が延びるとその間ほかのトリガーの tick が全部止まる。
    let deadline = std::time::Instant::now() + TOTAL_TIMEOUT;

    let mut resp = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(JsErrorBox::generic(format!(
                "http request timed out after {}s (including redirects)",
                TOTAL_TIMEOUT.as_secs()
            )));
        }

        let mut req = HTTP_CLIENT
            .request(method.clone(), current.clone())
            .timeout(remaining);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        if let Some(body) = &body {
            req = req.body(body.clone());
        }

        let resp = req
            .send()
            .await
            .map_err(|e| JsErrorBox::generic(format!("http request failed: {e}")))?;

        // 上限に当たったら追跡をやめ、3xx をそのままトリガーに返す。エラーにしないのは、
        // 「リダイレクトが返ってきた」こと自体は正当な応答だから。
        if hops >= MAX_REDIRECTS {
            break resp;
        }
        let Some((next, next_method)) = next_hop(&resp, &current, &method) else {
            break resp;
        };

        hops += 1;
        // 別ホストへ渡る前に認証情報を落とす。宣言の内側でも、片方向けの token を
        // もう片方に渡してよい理由は無い。
        strip_sensitive_headers_on_cross_host(&mut headers, &current, &next);
        current = next;
        // メソッドが書き換わった = GET に落ちたということなので body も落とす。
        if next_method != method {
            body = None;
        }
        method = next_method;
        // ホップ先も宣言の内側でなければならない。ここを飛ばすと、宣言済みホストが
        // 302 で任意の宛先を指すだけで allowedHosts が抜ける。
        authorize(&state, &snapshot, &current, true)?;
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{parse_host_pattern, TriggerGrants, TriggerPermissions};
    use std::collections::BTreeMap;

    fn snapshot_for(hosts: &[&str]) -> PermissionSnapshot {
        let grants = TriggerGrants {
            hosts: hosts
                .iter()
                .map(|h| parse_host_pattern(h).expect("valid pattern"))
                .collect(),
            ..Default::default()
        };
        let mut perms = TriggerPermissions::new(BTreeMap::from([("t".to_string(), grants)]));
        perms.enter("t");
        perms.snapshot()
    }

    /// 本番と同じ [`check`] を通す (URL → FetchTarget の翻訳もここの責務なので、
    /// テスト側で組み立て直すと肝心の翻訳が覆われない)。
    fn denial_for(snapshot: &PermissionSnapshot, url: &str) -> Result<(), String> {
        let parsed = reqwest::Url::parse(url).expect("valid url");
        check(snapshot, &parsed, false).map_err(|d| d.message)
    }

    /// URL からホストを取り出す経路が宣言と噛み合っていること。パスやクエリ、ポートは
    /// 宣言に含まれないので、それらが付いていても判断は変わらない。
    #[test]
    fn declared_host_is_allowed_regardless_of_path_and_port() {
        let s = snapshot_for(&["api.github.com"]);
        assert!(denial_for(&s, "https://api.github.com/search/issues?q=x").is_ok());
        assert!(denial_for(&s, "https://api.github.com:8443/x").is_ok());
    }

    #[test]
    fn undeclared_host_is_denied() {
        let s = snapshot_for(&["api.github.com"]);
        let err = denial_for(&s, "https://evil.example.com/collect").unwrap_err();
        assert!(err.contains("allowedHosts"), "{err}");
    }

    /// 末尾一致だけの実装だと `notgithub.com` が `github.com` の宣言で通ってしまう。
    #[test]
    fn suffix_lookalikes_are_denied() {
        let s = snapshot_for(&["github.com"]);
        assert!(denial_for(&s, "https://notgithub.com/x").is_err());
        assert!(denial_for(&s, "https://github.com.evil.test/x").is_err());
    }

    #[test]
    fn subdomain_wildcard_does_not_cover_the_apex() {
        let s = snapshot_for(&["*.githubusercontent.com"]);
        assert!(denial_for(&s, "https://raw.githubusercontent.com/a/b").is_ok());
        assert!(denial_for(&s, "https://deep.nested.githubusercontent.com/x").is_ok());
        assert!(denial_for(&s, "https://githubusercontent.com/x").is_err());
    }

    /// 宣言済みでも平文では出さない。ループバックだけが例外。
    #[test]
    fn plaintext_is_only_allowed_to_loopback() {
        let s = snapshot_for(&["api.github.com", "localhost"]);
        let err = denial_for(&s, "http://api.github.com/x").unwrap_err();
        assert!(err.contains("not https"), "{err}");
        assert!(denial_for(&s, "http://localhost:3000/x").is_ok());
    }

    fn headers_after_hop(from: &str, to: &str) -> Vec<String> {
        let mut headers = HashMap::from([
            ("Authorization".to_string(), "Bearer secret".to_string()),
            ("Cookie".to_string(), "session=1".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ]);
        strip_sensitive_headers_on_cross_host(
            &mut headers,
            &reqwest::Url::parse(from).expect("valid url"),
            &reqwest::Url::parse(to).expect("valid url"),
        );
        let mut names: Vec<String> = headers.into_keys().collect();
        names.sort();
        names
    }

    /// **宣言の内側でも**、別ホストへ渡るときは認証情報を落とす。reqwest の redirect policy を
    /// 切った時点でこれが落ちていた。`["api.github.com", "*.githubusercontent.com"]` は
    /// どちらも正当な宣言なので、`allowedHosts` はここでは守りにならない。
    #[test]
    fn credentials_do_not_survive_a_cross_host_redirect() {
        assert_eq!(
            headers_after_hop(
                "https://api.github.com/repos/x/tarball",
                "https://objects.githubusercontent.com/blob"
            ),
            vec!["Accept".to_string()]
        );
    }

    /// 同じホスト内の転送では落とさない (落とすと普通の API 利用が壊れる)。
    #[test]
    fn same_host_redirect_keeps_credentials() {
        assert_eq!(
            headers_after_hop("https://api.github.com/a", "https://api.github.com/b"),
            vec![
                "Accept".to_string(),
                "Authorization".to_string(),
                "Cookie".to_string()
            ]
        );
    }

    /// ポート違いも別オリジンとして扱う (reqwest の判定に合わせる)。
    #[test]
    fn different_port_counts_as_cross_host() {
        assert_eq!(
            headers_after_hop("https://example.com/a", "https://example.com:8443/b"),
            vec!["Accept".to_string()]
        );
    }

    /// 実行文脈の外からは、宣言の有無に関わらず出られない。
    #[test]
    fn outside_of_a_trigger_everything_is_denied() {
        let perms = TriggerPermissions::default();
        let err = denial_for(&perms.snapshot(), "https://api.github.com/x").unwrap_err();
        assert!(err.contains("outside of a trigger"), "{err}");
    }

    /// 本物の JS runtime から `chamberlain.http.fetch` を呼び、**宣言外は送信前に落ちる**
    /// ことを確かめる (#57 の完了条件)。
    ///
    /// 拒否はネットワークに出る前に決まるので、このテストは通信しない。許可される側は
    /// 実際に外へ出てしまうため、ここでは検証しない (判断そのものは上の unit test が覆う)。
    #[test]
    fn denied_fetch_rejects_in_js_and_is_recorded() {
        let mut runtime = rustyscript::Runtime::new(rustyscript::RuntimeOptions {
            extensions: vec![crate::secrets::chamberlain_ops::init()],
            ..Default::default()
        })
        .expect("failed to init JS runtime");
        {
            let op_state = runtime.deno_runtime().op_state();
            let mut op_state = op_state.borrow_mut();
            let grants = TriggerGrants {
                hosts: vec![parse_host_pattern("api.github.com").expect("valid")],
                ..Default::default()
            };
            let mut perms = TriggerPermissions::new(BTreeMap::from([("t".to_string(), grants)]));
            perms.enter("t");
            op_state.put(perms);
        }

        let module = rustyscript::Module::new(
            "denied-fetch.ts",
            r#"
            export async function tick() {
              try {
                await chamberlain.http.fetch("https://evil.example.com/collect");
                return "reached the network";
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
            result.contains("allowedHosts"),
            "宣言外の fetch が送信前に落ちていない: {result}"
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
}
