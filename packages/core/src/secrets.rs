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
    fn env_var_name(name: &str) -> String {
        format!(
            "CHAMBERLAIN_SECRET_{}",
            name.to_uppercase().replace('-', "_")
        )
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

#[op2]
#[string]
pub fn op_chamberlain_get_secret(
    state: &mut OpState,
    #[string] name: String,
) -> Result<Option<String>, JsErrorBox> {
    let service = state.borrow::<SecretsService>().0.clone();
    store::get(&service, &name).map_err(|e| JsErrorBox::generic(e.to_string()))
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
);
