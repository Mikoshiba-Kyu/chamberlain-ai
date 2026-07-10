use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, WindowEvent,
};
use tauri_plugin_notification::{NotificationExt, PermissionState};

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// Windows toast notifications require the app's AppUserModelId to be registered
// in the registry; installers normally do this via a Start Menu shortcut, but a
// portable single-exe distribution has to self-register it on first run instead.
// Without this, tauri-plugin-notification's show() silently no-ops on Windows.
#[cfg(windows)]
fn register_aumid(app_id: &str, display_name: &str) {
    use windows_registry::CURRENT_USER;
    if let Ok(key) = CURRENT_USER.create(format!(r"SOFTWARE\Classes\AppUserModelId\{app_id}")) {
        let _ = key.set_string("DisplayName", display_name);
    }
}

/// Shows a toast notification. This always runs natively in Rust — never
/// through the (permanently hidden, tray-only) webview — so it works
/// regardless of window visibility. The scheduler below and the tray's
/// manual "Send test notification" both go through this same path.
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

const TICK_INTERVAL: Duration = Duration::from_secs(10);

/// The periodic-execution primitive for this MVP demo: wakes up every
/// `TICK_INTERVAL`, decides something happened (here: nothing more than a
/// counter), and notifies. Lives entirely in Rust as a background async
/// task — Chamberlain's app logic, not just its OS-integration layer, since
/// a permanently-hidden Tauri window doesn't reliably run webview JS (see
/// project notes on the notification-plugin/window-visibility investigation).
fn spawn_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick_count: u64 = 0;
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            tick_count += 1;
            send_notification(
                &app,
                "Chamberlain",
                &format!(
                    "Tick #{tick_count} — still watching (every {}s)",
                    TICK_INTERVAL.as_secs()
                ),
            );
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .on_window_event(|window, event| {
            // Close-to-tray: intercept the window's close button so it hides
            // the window instead of terminating the process. The app only
            // exits via the tray's Quit item.
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

            spawn_scheduler(app.handle().clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
