use std::path::PathBuf;
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

/// The value shape Chamberlain expects a TS trigger's `check()` to return:
/// either `null` (do nothing this tick) or `{ message: string }` (fire).
#[derive(Deserialize)]
struct CheckResult {
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

fn fire_trigger(app: &AppHandle, source: &str, message: String) {
    send_notification(app, "Chamberlain", &message);
    let _ = app.emit(
        "activity",
        ActivityEvent {
            ts: now_millis(),
            source: source.into(),
            message,
        },
    );
}

/// Spawn the sample trigger. The check logic lives in `triggers/sample-10s.ts`
/// and is evaluated by an embedded JS runtime (deno_core via rustyscript).
///
/// Threading: V8 isolates have thread affinity, so the `Runtime` is owned by
/// a dedicated OS thread. The tokio side just produces tick signals over a
/// channel; the JS thread does the actual `check()` call.
fn spawn_sample_trigger(app: AppHandle, paused: Arc<AtomicBool>) {
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

        let module_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("triggers")
            .join("sample-10s.ts");

        let module = match rustyscript::Module::load(&module_path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("failed to load trigger module {module_path:?}: {e}");
                return;
            }
        };

        let handle = match runtime.load_module(&module) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("failed to instantiate trigger module: {e}");
                return;
            }
        };

        while tick_rx.recv().is_ok() {
            if paused.load(Ordering::Relaxed) {
                continue;
            }
            let result: Result<Option<CheckResult>, _> =
                runtime.call_function(Some(&handle), "check", rustyscript::json_args!());
            match result {
                Ok(Some(res)) => fire_trigger(&app_for_worker, SAMPLE_TRIGGER_ID, res.message),
                Ok(None) => {}
                Err(e) => eprintln!("trigger check() error: {e}"),
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
