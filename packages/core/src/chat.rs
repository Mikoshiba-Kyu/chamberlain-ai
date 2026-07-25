//! Type II 秘書 chat の実装。
//!
//! - 会話履歴は `tauri-plugin-store` の別ファイル `chat-history.json` に保存
//! - 毎回全履歴を Anthropic に送る。長さは `MAX_HISTORY` で頭からドロップ
//! - system prompt はハードコード (将来 Settings で編集可にする、docs 未確定の論点)
//! - Type I トリガーと同じ `anthropic_api_key` を使う (secret store 経由)

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_store::StoreExt;

use crate::ai;
use crate::secrets::{store as secret_store, SecretsService, ANTHROPIC_API_KEY_NAME};

const CHAT_STORE_FILE: &str = "chat-history.json";
const CHAT_MESSAGES_KEY: &str = "messages";
const MAX_HISTORY: usize = 40;

const CHAMBERLAIN_SYSTEM_PROMPT: &str = "\
You are Chamberlain, a personal AI secretary. You help the user manage \
their tasks, notifications, and daily routines through this desktop app. \
Respond in the same language the user writes in. Keep replies concise and \
polite. When you don't know something, say so plainly.";

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant"
    pub content: String,
    pub ts: u64,
}

fn load_history(app: &AppHandle) -> Vec<ChatMessage> {
    let Ok(store) = app.store(CHAT_STORE_FILE) else {
        return Vec::new();
    };
    let value = store
        .get(CHAT_MESSAGES_KEY)
        .unwrap_or_else(|| serde_json::json!([]));
    match serde_json::from_value::<Vec<ChatMessage>>(value.clone()) {
        Ok(msgs) => msgs,
        Err(e) => {
            // 破損を silent に消さず、別キーに退避してから空扱いにする。
            // これを入れないと次の save で完全上書きされて復旧手段が消える。
            let backup_key = format!("messages_corrupted_{}", crate::now_millis());
            eprintln!("chat history parse error: {e}; preserving corrupt copy at '{backup_key}'");
            store.set(&backup_key, value);
            let _ = store.save();
            Vec::new()
        }
    }
}

fn save_history(app: &AppHandle, msgs: &[ChatMessage]) -> Result<(), String> {
    let store = app.store(CHAT_STORE_FILE).map_err(|e| e.to_string())?;
    store.set(
        CHAT_MESSAGES_KEY,
        serde_json::to_value(msgs).map_err(|e| e.to_string())?,
    );
    store.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn chat_history(app: AppHandle) -> Vec<ChatMessage> {
    load_history(&app)
}

#[tauri::command]
pub fn chat_clear(app: AppHandle) -> Result<(), String> {
    save_history(&app, &[])
}

#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    message: String,
    secrets: State<'_, SecretsService>,
) -> Result<ChatMessage, String> {
    // await 越しに State を保持しないよう先に取り出す
    let service_name = secrets.0.clone();

    let api_key = secret_store::get(&service_name, ANTHROPIC_API_KEY_NAME)
        .map_err(|e| format!("secret store error: {e}"))?
        .ok_or_else(|| "anthropic_api_key is not set".to_string())?;

    let mut history = load_history(&app);

    let user_msg = ChatMessage {
        role: "user".to_string(),
        content: message,
        ts: crate::now_millis(),
    };
    history.push(user_msg);

    // 履歴が MAX_HISTORY を超えたら古い方から drop。会話の対称性は捨てる (system
    // prompt が context を補うので許容する)。
    while history.len() > MAX_HISTORY {
        history.remove(0);
    }

    // AI 呼び出し前に user メッセージを永続化。API キー未設定や API 側の失敗で
    // `?` return したとき、ユーザーが送った文言まで巻き添えで消えるのを防ぐ。
    save_history(&app, &history)?;

    let ai_messages: Vec<ai::Message> = history
        .iter()
        .map(|m| ai::Message {
            role: match m.role.as_str() {
                "assistant" => ai::Role::Assistant,
                _ => ai::Role::User,
            },
            content: m.content.clone(),
        })
        .collect();

    let response = ai::complete(
        &api_key,
        None,
        Some(CHAMBERLAIN_SYSTEM_PROMPT),
        &ai_messages,
    )
    .await?;

    let assistant_msg = ChatMessage {
        role: "assistant".to_string(),
        content: response,
        ts: crate::now_millis(),
    };
    history.push(assistant_msg.clone());
    save_history(&app, &history)?;

    Ok(assistant_msg)
}
