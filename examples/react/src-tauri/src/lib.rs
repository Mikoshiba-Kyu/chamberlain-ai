/// エージェント開発者が書く app エントリ。フレームワーク本体は `chamberlain_core::builder()`
/// が組み立て済みの Tauri Builder を返す。ここでは `.run(tauri::generate_context!())` を
/// 呼ぶだけ (`tauri.conf.json` 参照のため app crate 側必須)。
///
/// トリガーの探索先は `tauri.conf.json` の `bundle.resources` で宣言されたパスから
/// core が resource dir 経由で解決する (#19)。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    chamberlain_core::builder()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
