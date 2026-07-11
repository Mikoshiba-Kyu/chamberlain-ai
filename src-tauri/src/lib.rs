use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tauri_plugin_store::StoreExt;

const TICK_INTERVAL: Duration = Duration::from_secs(10);
const STATE_STORE_FILE: &str = "triggers-state.json";

/// Activity event emitted whenever a trigger fires. This is the primary
/// observability surface Chamberlain exposes to its UI — see issue #6
/// ("UI as observability plane"): every trigger firing, notification, or
/// proactive action must also arrive here so the developer can watch the
/// secretary's behavior without depending on OS-level notification rendering.
#[derive(Clone, Serialize)]
struct ActivityEvent {
    ts: u64,
    source: String,
    message: String,
}

#[derive(Deserialize)]
struct NotifyPayload {
    message: String,
}

#[derive(Deserialize, Default)]
struct TickResult {
    #[serde(default)]
    notify: Option<NotifyPayload>,
    #[serde(default)]
    state: Option<serde_json::Value>,
}

#[derive(Clone, Deserialize)]
struct TriggerManifest {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
    entry: String,
}

struct TriggerInfo {
    manifest: TriggerManifest,
    dir: PathBuf,
    paused: Arc<AtomicBool>,
}

type TriggersRef = Arc<Vec<TriggerInfo>>;

/// UI が受け取るトリガー一覧の要素。manifest 由来 + 現在の paused 状態。
#[derive(Serialize)]
struct TriggerListItem {
    id: String,
    name: String,
    description: Option<String>,
    paused: bool,
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(windows)]
fn register_aumid(app_id: &str, display_name: &str) {
    use windows_registry::CURRENT_USER;
    if let Ok(key) = CURRENT_USER.create(format!(r"SOFTWARE\Classes\AppUserModelId\{app_id}")) {
        let _ = key.set_string("DisplayName", display_name);
    }
}

fn send_notification(app: &AppHandle, title: &str, body: &str) {
    let notification = app.notification();

    let granted = match notification.permission_state() {
        Ok(PermissionState::Granted) => true,
        Ok(_) => matches!(notification.request_permission(), Ok(PermissionState::Granted)),
        Err(_) => false,
    };

    if granted {
        let _ = notification.builder().title(title).body(body).show();
    } else {
        eprintln!("notification permission not granted");
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn emit_activity(app: &AppHandle, source: &str, message: String) {
    let _ = app.emit(
        "activity",
        ActivityEvent {
            ts: now_millis(),
            source: source.into(),
            message,
        },
    );
}

fn fire_trigger(app: &AppHandle, source: &str, message: String) {
    send_notification(app, "Chamberlain", &message);
    emit_activity(app, source, message);
}

fn read_trigger_state(app: &AppHandle, trigger_id: &str) -> serde_json::Value {
    match app.store(STATE_STORE_FILE) {
        Ok(store) => store
            .get(trigger_id)
            .unwrap_or_else(|| serde_json::json!({})),
        Err(e) => {
            eprintln!("failed to open state store: {e}");
            serde_json::json!({})
        }
    }
}

fn write_trigger_state(app: &AppHandle, trigger_id: &str, state: serde_json::Value) {
    match app.store(STATE_STORE_FILE) {
        Ok(store) => {
            store.set(trigger_id, state);
            if let Err(e) = store.save() {
                eprintln!("failed to persist state for {trigger_id}: {e}");
            }
        }
        Err(e) => eprintln!("failed to open state store for write: {e}"),
    }
}

/// `triggers/*/manifest.json` を走査して有効なトリガーだけを拾う。
/// - manifest 読み取り失敗 / JSON 不正 → その 1 個をスキップ、他は続行
/// - id 重複 → 先勝ち、後発をスキップして log
/// - 実行順序を安定させるため id 昇順にソート
fn discover_triggers(triggers_dir: &Path) -> Vec<TriggerInfo> {
    let entries = match std::fs::read_dir(triggers_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("failed to read triggers dir {triggers_dir:?}: {e}");
            return Vec::new();
        }
    };

    let mut result: Vec<TriggerInfo> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("failed to read {manifest_path:?}: {e}");
                continue;
            }
        };
        let manifest: TriggerManifest = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("invalid manifest {manifest_path:?}: {e}");
                continue;
            }
        };
        result.push(TriggerInfo {
            manifest,
            dir: path,
            paused: Arc::new(AtomicBool::new(false)),
        });
    }

    let mut seen = HashSet::new();
    let mut deduped: Vec<TriggerInfo> = Vec::new();
    for t in result {
        if seen.insert(t.manifest.id.clone()) {
            deduped.push(t);
        } else {
            eprintln!("duplicate trigger id '{}', skipping", t.manifest.id);
        }
    }

    deduped.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    deduped
}

/// JS ワーカー: 単一の rustyscript Runtime に N モジュールを載せ、tick 毎に順番に
/// tick() を呼ぶ。V8 の thread affinity を守るため、Runtime はこの std::thread に閉じ込め、
/// tokio 側からは mpsc で tick を送るだけ。
///
/// Per-tick per-trigger 順序: paused判定 → state読 → tick(ctx) → notify → state保存。
/// notify が state 保存より先。プロセスクラッシュ時の "at least once" を優先 (秘書は
/// 「1回多く言う > 一言忘れる」)。
fn spawn_trigger_worker(app: AppHandle, triggers: TriggersRef) {
    let (tick_tx, tick_rx) = mpsc::channel::<()>();
    let app_for_worker = app.clone();

    std::thread::spawn(move || {
        let mut runtime = match rustyscript::Runtime::new(rustyscript::RuntimeOptions::default()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed to init JS runtime: {e}");
                return;
            }
        };

        // 起動時に全モジュールをロード。ロード失敗したものはスキップ (他トリガーは動く)。
        let mut loaded: Vec<(usize, rustyscript::ModuleHandle)> = Vec::new();
        for (idx, t) in triggers.iter().enumerate() {
            let entry_path = t.dir.join(&t.manifest.entry);
            let module = match rustyscript::Module::load(&entry_path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "failed to load trigger '{}' at {:?}: {e}",
                        t.manifest.id, entry_path
                    );
                    emit_activity(
                        &app_for_worker,
                        &t.manifest.id,
                        format!("[load error] {e}"),
                    );
                    continue;
                }
            };
            let handle = match runtime.load_module(&module) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("failed to instantiate trigger '{}': {e}", t.manifest.id);
                    emit_activity(
                        &app_for_worker,
                        &t.manifest.id,
                        format!("[instantiate error] {e}"),
                    );
                    continue;
                }
            };
            loaded.push((idx, handle));
        }

        while tick_rx.recv().is_ok() {
            for (idx, handle) in &loaded {
                let trigger = &triggers[*idx];
                if trigger.paused.load(Ordering::Relaxed) {
                    continue;
                }
                let id = &trigger.manifest.id;
                let current_state = read_trigger_state(&app_for_worker, id);
                let ctx = serde_json::json!({
                    "now": now_millis(),
                    "state": current_state,
                });
                let result: Result<Option<TickResult>, _> =
                    runtime.call_function(Some(handle), "tick", rustyscript::json_args!(ctx));
                match result {
                    Ok(Some(res)) => {
                        if let Some(notify) = res.notify {
                            fire_trigger(&app_for_worker, id, notify.message);
                        }
                        if let Some(new_state) = res.state {
                            write_trigger_state(&app_for_worker, id, new_state);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("trigger '{}' tick() error: {e}", id);
                        emit_activity(&app_for_worker, id, format!("[error] {e}"));
                    }
                }
            }
        }
    });

    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            if tick_tx.send(()).is_err() {
                break;
            }
        }
    });
}

#[tauri::command]
fn list_triggers(triggers: State<'_, TriggersRef>) -> Vec<TriggerListItem> {
    triggers
        .iter()
        .map(|t| TriggerListItem {
            id: t.manifest.id.clone(),
            name: t.manifest.name.clone(),
            description: t.manifest.description.clone(),
            paused: t.paused.load(Ordering::Relaxed),
        })
        .collect()
}

#[tauri::command]
fn pause_trigger(id: String, triggers: State<'_, TriggersRef>) -> Result<(), String> {
    match triggers.iter().find(|t| t.manifest.id == id) {
        Some(t) => {
            t.paused.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("unknown trigger: {id}")),
    }
}

#[tauri::command]
fn resume_trigger(id: String, triggers: State<'_, TriggersRef>) -> Result<(), String> {
    match triggers.iter().find(|t| t.manifest.id == id) {
        Some(t) => {
            t.paused.store(false, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("unknown trigger: {id}")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            list_triggers,
            pause_trigger,
            resume_trigger,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(|app| {
            #[cfg(windows)]
            {
                let identifier = app.config().identifier.clone();
                let display_name = app
                    .config()
                    .product_name
                    .clone()
                    .unwrap_or_else(|| identifier.clone());
                register_aumid(&identifier, &display_name);
            }

            let open_item = MenuItem::with_id(app, "open", "Open Chamberlain", true, None::<&str>)?;
            let notify_item = MenuItem::with_id(app, "notify", "Send test notification", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &notify_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "notify" => send_notification(app, "Chamberlain", "テスト通知です"),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            let triggers_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("triggers");
            let triggers: TriggersRef = Arc::new(discover_triggers(&triggers_dir));
            for t in triggers.iter() {
                eprintln!(
                    "discovered trigger: {} ({}) — entry {}",
                    t.manifest.id, t.manifest.name, t.manifest.entry
                );
            }
            app.manage(triggers.clone());
            spawn_trigger_worker(app.handle().clone(), triggers);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
