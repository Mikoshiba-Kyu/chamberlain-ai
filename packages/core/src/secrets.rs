//! OS credential manager 経由の secret store。
//!
//! - Windows: Credential Manager
//! - macOS: Keychain
//! - Linux: Secret Service (要 dbus / gnome-keyring or KWallet)
//!
//! keyring クレートの薄いラッパ + Tauri commands (React UI 用) + deno_core op
//! (`chamberlain.getSecret(...)` の実装) を一箇所にまとめている。
//!
//! Linux devcontainer には dbus セッションが無いため、実行時に keyring 呼び出しが
//! 失敗する。これは既知 (docs 参照)。実 Windows / macOS / WSLg 環境が本番検証対象。

use deno_core::{extension, op2, OpState};
use deno_error::JsErrorBox;
use tauri::State;

use crate::permissions::TriggerPermissions;

/// keyring の service 名として使うアプリ識別子 (tauri.conf.json の `identifier`)。
/// Tauri state と OpState の両方に格納する。
#[derive(Clone)]
pub struct SecretsService(pub String);

/// framework-required secret: Chamberlain 本体の Type II 秘書 AI と、共通の
/// `chamberlain.ai.complete` op がここから API キーを引く。設定 UI に必ず現れる。
pub const ANTHROPIC_API_KEY_NAME: &str = "anthropic_api_key";

pub mod store {
    use keyring::{Entry, Error};

    fn entry(service: &str, name: &str) -> Result<Entry, Error> {
        Entry::new(service, name)
    }

    /// name を env var 名に変換する。`github_token` -> `CHAMBERLAIN_SECRET_GITHUB_TOKEN`。
    ///
    /// env var 名として有効な `[A-Z0-9_]` 以外の文字 (`-` / `.` / 空白 / 多バイト文字等) は
    /// すべて `_` に丸める。旧実装は `-` だけを潰していたため、`github.token` のような
    /// 名前で env fallback が silent に効かなかった (Issue #21 #11)。
    ///
    /// **これは多対 1 の写像である。** `anthropic_api_key` / `ANTHROPIC-API-KEY` /
    /// `anthropic.api.key` は同じ env var に落ちる = **同じ secret を指す**。名前の綴りで
    /// 権限を判定すると別綴りで素通りするので、[`crate::permissions`] はこの関数を
    /// 通してから比較する。
    pub(crate) fn env_var_name(name: &str) -> String {
        let mut out = String::with_capacity("CHAMBERLAIN_SECRET_".len() + name.len());
        out.push_str("CHAMBERLAIN_SECRET_");
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_uppercase());
            } else {
                out.push('_');
            }
        }
        out
    }

    /// 未設定 (NoEntry) は `Ok(None)`、その他のエラーは `Err`。
    ///
    /// **env-var fallback**: keyring を叩く前に `CHAMBERLAIN_SECRET_<UPPERCASE>` を
    /// 見る。dev 環境で `.env` 経由で secret を注入したいときの逃げ道。keyring 環境で
    /// セットしなければ透過 (従来通り)。builder() 起動時に dotenvy が呼ばれるので、
    /// `.env` に書けば自動でロードされる。
    pub fn get(service: &str, name: &str) -> Result<Option<String>, Error> {
        if let Ok(v) = std::env::var(env_var_name(name)) {
            if !v.is_empty() {
                return Ok(Some(v));
            }
        }
        match entry(service, name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn set(service: &str, name: &str, value: &str) -> Result<(), Error> {
        entry(service, name)?.set_password(value)
    }

    /// 存在しないエントリの削除は成功扱い (`Ok(())`)。env-var 側は触らない
    /// (プロセス外の設定なのでコマンド経由で削除できない)。
    pub fn delete(service: &str, name: &str) -> Result<(), Error> {
        match entry(service, name)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(e),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::env_var_name;

        #[test]
        fn env_var_name_basic() {
            assert_eq!(
                env_var_name("anthropic_api_key"),
                "CHAMBERLAIN_SECRET_ANTHROPIC_API_KEY"
            );
        }

        #[test]
        fn env_var_name_replaces_non_alnum() {
            assert_eq!(
                env_var_name("github-token"),
                "CHAMBERLAIN_SECRET_GITHUB_TOKEN"
            );
            assert_eq!(
                env_var_name("github.token"),
                "CHAMBERLAIN_SECRET_GITHUB_TOKEN"
            );
            assert_eq!(
                env_var_name("github token"),
                "CHAMBERLAIN_SECRET_GITHUB_TOKEN"
            );
        }

        #[test]
        fn env_var_name_multibyte() {
            // マルチバイト char (JIS 用途で名前をつける事故想定) も 1 char = 1 `_` に潰す
            assert_eq!(env_var_name("トークン"), "CHAMBERLAIN_SECRET_____");
        }
    }
}

// --- Tauri commands (React UI から呼ばれる) ---

#[tauri::command]
pub fn set_secret(
    name: String,
    value: String,
    service: State<'_, SecretsService>,
) -> Result<(), String> {
    store::set(&service.0, &name, &value).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn has_secret(name: String, service: State<'_, SecretsService>) -> Result<bool, String> {
    store::get(&service.0, &name)
        .map(|opt| opt.is_some())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_secret(name: String, service: State<'_, SecretsService>) -> Result<(), String> {
    store::delete(&service.0, &name).map_err(|e| e.to_string())
}

// --- deno_core op (JS runtime から `chamberlain.getSecret(name)` として呼ばれる) ---

/// 呼び出し元トリガーの manifest 宣言 (`requiredSecrets`) に無い名前は `null` を返す (#56)。
///
/// **例外ではなく `null`** にしているのは、未設定の secret と同じ形にするため。トリガー側は
/// 既に「null なら諦める」を書いているはずで、拒否のためだけに別の分岐を書かせる理由が無い。
/// 代わりに拒否は観測面 (`[denied]`) に残るので、エージェント開発者は宣言し忘れに気付ける。
///
/// 判断そのものは [`crate::permissions`] にある。[`TriggerPermissions`] は extension が
/// 起動時に必ず載せる (下の `state =`) ので、ここでは在ることを前提にしてよい。
#[op2]
#[string]
pub fn op_chamberlain_get_secret(
    state: &mut OpState,
    #[string] name: String,
) -> Result<Option<String>, JsErrorBox> {
    if !state
        .borrow_mut::<TriggerPermissions>()
        .authorize_secret(&name)
    {
        return Ok(None);
    }
    let service = state.borrow::<SecretsService>().0.clone();
    store::get(&service, &name).map_err(|e| JsErrorBox::generic(e.to_string()))
}

#[cfg(test)]
mod op_tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::permissions::TriggerGrants;

    /// テストの間だけ env var を差し替え、drop で元に戻す。
    ///
    /// env はプロセス共有で、テストは同一バイナリ内で並行に走る。値を残すと後続のテスト
    /// (特に `store::get` の env fallback を通るもの) の結果を変えてしまう。
    /// `schedule.rs` の TZ テストと同じ save/restore を、複数変数向けに RAII にしたもの。
    struct EnvGuard(Vec<(String, Option<String>)>);

    impl EnvGuard {
        fn set(vars: &[(&str, &str)]) -> Self {
            Self(
                vars.iter()
                    .map(|(k, v)| {
                        let saved = std::env::var(k).ok();
                        // SAFETY: shared env; 触るのは CHAMBERLAIN_SECRET_* だけで、
                        // 他テストはこれらを読まない。drop で必ず元に戻す。
                        unsafe { std::env::set_var(k, v) };
                        ((*k).to_string(), saved)
                    })
                    .collect(),
            )
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, saved) in &self.0 {
                // SAFETY: set() と同じ。
                match saved {
                    Some(v) => unsafe { std::env::set_var(k, v) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }

    /// 本物の JS runtime を立てて `chamberlain.getSecret` の 4 経路を通す。
    ///
    /// [`crate::permissions`] の unit test は判断そのものを覆うが、**op と bootstrap.js の
    /// 配線**はそこには乗らない。#56 の完了条件は「JS から見て null が返ること」なので、
    /// 一度だけ実物を通しておく。
    ///
    /// keyring は devcontainer に dbus が無くて叩けないので、`CHAMBERLAIN_SECRET_*` の
    /// env fallback で値を用意する (`store::get` の実装どおり)。1 テストに 4 経路を
    /// まとめてあるのは、V8 の isolate をテストごとに立てないため。
    #[test]
    fn get_secret_is_scoped_to_the_declaring_trigger() {
        // env はプロセス共有なので、他テストに値を残さないよう必ず戻す。
        let _env = EnvGuard::set(&[
            ("CHAMBERLAIN_SECRET_GITHUB_TOKEN", "declared-value"),
            ("CHAMBERLAIN_SECRET_SLACK_TOKEN", "undeclared-value"),
            ("CHAMBERLAIN_SECRET_ANTHROPIC_API_KEY", "framework-value"),
        ]);

        let mut runtime = rustyscript::Runtime::new(rustyscript::RuntimeOptions {
            extensions: vec![chamberlain_ops::init()],
            ..Default::default()
        })
        .expect("failed to init JS runtime");
        {
            let op_state = runtime.deno_runtime().op_state();
            let mut op_state = op_state.borrow_mut();
            // extension が既定の権限を載せていること。op はこれを無条件に borrow するので、
            // 載っていなければ「配線し忘れが素通り」ではなく panic になる。
            assert!(op_state.has::<TriggerPermissions>());
            op_state.put(SecretsService("chamberlain-test".to_string()));
            op_state.put(TriggerPermissions::new(BTreeMap::from([(
                "declaring".to_string(),
                // framework のキーを 2 通りの綴りで宣言させる。どちらも渡らないのが仕様。
                // 別綴り (ANTHROPIC-API-KEY) は env fallback の正規化で同じ値に解決する
                // ので、綴り一致で弾いていた頃はここから素通りしていた。
                TriggerGrants {
                    secrets: ["github_token", ANTHROPIC_API_KEY_NAME, "ANTHROPIC-API-KEY"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    ..Default::default()
                },
            )])));
        }

        let module = rustyscript::Module::new(
            "scoped-secrets.ts",
            r#"
            export async function tick() {
              return {
                declared: await chamberlain.getSecret("github_token"),
                undeclared: await chamberlain.getSecret("slack_token"),
                framework: await chamberlain.getSecret("anthropic_api_key"),
                frameworkAlias: await chamberlain.getSecret("ANTHROPIC-API-KEY"),
              };
            }
            "#,
        );
        let handle = runtime.load_module(&module).expect("failed to load module");

        // 本番では TauriHost::run_js がこれを挟む。
        {
            let op_state = runtime.deno_runtime().op_state();
            op_state
                .borrow_mut()
                .borrow_mut::<TriggerPermissions>()
                .enter("declaring");
        }
        let result: serde_json::Value = runtime
            .call_function(Some(&handle), "tick", rustyscript::json_args!())
            .expect("tick failed");

        assert_eq!(result["declared"], serde_json::json!("declared-value"));
        assert_eq!(result["undeclared"], serde_json::Value::Null);
        assert_eq!(result["framework"], serde_json::Value::Null);
        assert_eq!(
            result["frameworkAlias"],
            serde_json::Value::Null,
            "別綴りで宣言しても framework のキーは渡らない"
        );

        let op_state = runtime.deno_runtime().op_state();
        let denials = op_state
            .borrow_mut()
            .borrow_mut::<TriggerPermissions>()
            .leave();
        assert_eq!(denials.len(), 3, "拒否は 3 件とも観測面に残る: {denials:?}");
        assert!(denials
            .iter()
            .all(|d| d.trigger_id.as_deref() == Some("declaring")));
        assert!(denials[0].message.contains("slack_token"));
        assert!(denials[1].message.contains("chamberlain.ai.complete"));
        assert!(denials[2].message.contains("ANTHROPIC-API-KEY"));
    }
}

extension!(
    chamberlain_ops,
    ops = [
        op_chamberlain_get_secret,
        crate::ai::op_chamberlain_ai_complete,
        crate::http::op_chamberlain_http_fetch,
    ],
    esm_entry_point = "ext:chamberlain_ops/bootstrap.js",
    esm = [dir "src", "bootstrap.js"],
    // 権限の状態は ops と一緒に載せる。既定値は「宣言ゼロ・実行文脈なし」= 全部拒否なので、
    // worker が manifest 由来の宣言で差し替え損ねても素通りにならない (#56)。
    state = |state: &mut OpState| state.put(TriggerPermissions::default()),
);
