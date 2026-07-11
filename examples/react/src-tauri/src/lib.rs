use std::path::PathBuf;

/// エージェント開発者が書く app エントリ。フレームワーク本体は `chamberlain_core::builder`
/// が組み立て済みの Tauri Builder を返す。ここでは:
///   - トリガーディレクトリの位置を解決する (dev では `env!("CARGO_MANIFEST_DIR")` からの相対)
///   - `.run(tauri::generate_context!())` を呼ぶ (`tauri.conf.json` 参照のため app crate 側必須)
///
/// shipped ビルド時のトリガー位置解決はエージェント開発者側で切り替える (docs/architecture.md
/// 「未確定の論点」参照)。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let triggers_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("triggers");

    chamberlain_core::builder(chamberlain_core::ChamberlainConfig { triggers_dir })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
