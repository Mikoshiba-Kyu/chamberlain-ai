//! Type II 秘書 chat の実装。
//!
//! - 会話履歴は `tauri-plugin-store` の別ファイル `chat-history.json` に保存
//! - 毎回全履歴を Anthropic に送る。長さは `MAX_HISTORY` で頭からドロップ
//! - system prompt はハードコード (将来 Settings で編集可にする、docs 未確定の論点)
//! - Type I トリガーと同じ `anthropic_api_key` を使う (secret store 経由)
//! - 道具を 1 つだけ持つ (#61)。「繰り返しやってほしいこと」を頼まれたら、秘書が自分で
//!   判断してトリガーの下書きを作る。詳細は [`crate::drafts`]

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_store::StoreExt;

use crate::drafts::{self, DraftDir};
use crate::secrets::{store as secret_store, SecretsService, ANTHROPIC_API_KEY_NAME};
use crate::{ai, HistoryRef, RegisteredDir, TriggerCandidate, TriggersRef};

const CHAT_STORE_FILE: &str = "chat-history.json";
const CHAT_MESSAGES_KEY: &str = "messages";
const MAX_HISTORY: usize = 40;

/// 秘書の persona と、道具をいつ使うかの判断基準。
///
/// **「1 回きりか、繰り返しか」を秘書に判断させている** (#61 論点 1)。入口はどちらも
/// 同じチャットなので、機構では分けられない。ad-hoc タスク (#43) はまだ無いので、
/// 1 回きりの用事は `@at` のトリガーとして表現するか、その場で答えるかになる。
const CHAMBERLAIN_SYSTEM_PROMPT: &str = "\
You are Chamberlain, a personal AI secretary. You help the user manage \
their tasks, notifications, and daily routines through this desktop app. \
Respond in the same language the user writes in. Keep replies concise and \
polite. When you don't know something, say so plainly.

When the user asks you to check something or tell them something on a \
recurring schedule (\"every morning at 9\", \"hourly\", \"every Friday\"), call the \
propose_trigger tool instead of answering in prose. A one-off future reminder \
(\"remind me next Monday\") can also be a trigger scheduled with @at. Do not use \
the tool for questions you can simply answer, or for things the user wants done \
right now.";

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant"
    pub content: String,
    pub ts: u64,
}

/// `chat_send` の戻り値。
///
/// **返答と下書きを 1 つの型にまとめる。** 下書きができたことを別の口 (イベントや
/// 再取得) で知らせると、「秘書が作りましたと言っているのにカードが出ない」瞬間が
/// 生まれる。作った本人の返答と同時に届く形にしておく。
#[derive(Serialize)]
pub struct ChatTurn {
    pub message: ChatMessage,
    /// 秘書が用意したトリガーの下書き (#61)。**まだ何も登録されていない。**
    /// UI はこれを #58 と同じ同意画面に出し、確認が取れたら `register_trigger` を呼ぶ。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<TriggerCandidate>,
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
    triggers: State<'_, TriggersRef>,
    registered: State<'_, RegisteredDir>,
    draft_dir: State<'_, DraftDir>,
    history_store: State<'_, HistoryRef>,
) -> Result<ChatTurn, String> {
    // await 越しに State を保持しないよう先に取り出す
    let service_name = secrets.0.clone();
    let triggers: TriggersRef = Arc::clone(&triggers);
    let history_store: HistoryRef = Arc::clone(&history_store);
    // 置き場が解決できない環境では下書きが作れないだけで、会話は続けられる。
    let registered_dir: Option<PathBuf> = registered.get().ok().map(|d| d.to_path_buf());
    let draft_root: Option<PathBuf> = draft_dir.get().ok().map(|d| d.to_path_buf());

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

    let response = ai::complete_with_tools(
        &api_key,
        None,
        Some(CHAMBERLAIN_SYSTEM_PROMPT),
        &ai_messages,
        &[drafts::propose_trigger_tool()],
    )
    .await?;

    let (content, draft) = match response {
        ai::Completion::Text(text) => (text, None),
        // 呼ばれた道具が知らない名前だったときも、会話は続ける。モデルの言い間違いで
        // チャットが使えなくなる方が実害が大きい。
        ai::Completion::ToolUse { text, name, .. } if name != drafts::PROPOSE_TRIGGER_TOOL => {
            eprintln!("chat: ignoring unknown tool '{name}'");
            (
                text.unwrap_or_else(|| "うまく応答できませんでした。".to_string()),
                None,
            )
        }
        ai::Completion::ToolUse { text, input, .. } => {
            let site = drafts::DraftSite {
                root: draft_root.as_deref(),
                triggers: &triggers,
                registered_dir: registered_dir.as_deref(),
            };
            let outcome = drafts::propose(&app, &history_store, &api_key, &input, site).await;
            describe_proposal(text, outcome)
        }
    };

    let assistant_msg = ChatMessage {
        role: "assistant".to_string(),
        content,
        ts: crate::now_millis(),
    };
    history.push(assistant_msg.clone());
    save_history(&app, &history)?;

    Ok(ChatTurn {
        message: assistant_msg,
        draft,
    })
}

/// 下書きの結果を秘書の言葉にする。
///
/// **生成の失敗は会話の失敗にしない。** `?` で返すと、ユーザーの発言だけが履歴に残って
/// 秘書が黙ったように見える。
fn describe_proposal(
    preamble: Option<String>,
    outcome: Result<TriggerCandidate, String>,
) -> (String, Option<TriggerCandidate>) {
    match outcome {
        Ok(candidate) => {
            let preamble =
                preamble.unwrap_or_else(|| "ご依頼のトリガーを用意しました。".to_string());
            (
                format!("{preamble}\n\n内容を確認して、よろしければ登録してください。"),
                Some(candidate),
            )
        }
        Err(e) => (
            format!(
                "トリガーを用意できませんでした: {e}\n\nもう少し具体的に教えていただけますか。"
            ),
            None,
        ),
    }
}
