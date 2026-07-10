use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_notification::{NotificationExt, PermissionState};

const SAMPLE_TRIGGER_ID: &str = "sample-10s";
const TICK_INTERVAL: Duration = Duration::from_secs(10);

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

struct AppState {
    sample_paused: Arc<AtomicBool>,
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

fn spawn_sample_trigger(app: AppHandle, paused: Arc<AtomicBool>) {
    tauri::async_runtime::spawn(async move {
        let mut tick_count: u64 = 0;
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            if paused.load(Ordering::Relaxed) {
                continue;
            }
            tick_count += 1;
            let message = format!("Tick #{tick_count}");
            send_notification(&app, "Chamberlain", &message);
            let event = ActivityEvent {
                ts: now_millis(),
                source: SAMPLE_TRIGGER_ID.into(),
                message,
            };
            let _ = app.emit("activity", event);
        }
    });
}

#[tauri::command]
fn pause_sample_trigger(state: State<'_, AppState>) {
    state.sample_paused.store(true, Ordering::Relaxed);
}

#[tauri::command]
fn resume_sample_trigger(state: State<'_, AppState>) {
    state.sample_paused.store(false, Ordering::Relaxed);
}

#[tauri::command]
fn sample_trigger_status(state: State<'_, AppState>) -> bool {
    state.sample_paused.load(Ordering::Relaxed)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let sample_paused = Arc::new(AtomicBool::new(false));

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            sample_paused: sample_paused.clone(),
        })
        .invoke_handler(tauri::generate_handler![
            pause_sample_trigger,
            resume_sample_trigger,
            sample_trigger_status,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(move |app| {
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

            spawn_sample_trigger(app.handle().clone(), sample_paused.clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
