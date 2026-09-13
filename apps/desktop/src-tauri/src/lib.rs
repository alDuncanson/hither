//! hither for the menu bar.
//!
//! Thin on purpose: every button calls a command in [`commands`], every
//! command calls `hither-core`, and every core [`hither_core::Event`] is
//! forwarded to the window as a Tauri event named `hither-event`. This file
//! owns the parts that are about being a Mac app: the tray icon, hiding
//! instead of quitting, and the `hither://` URL scheme.

mod commands;

use tauri::{
    AppHandle, Emitter, Manager, WindowEvent,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_deep_link::DeepLinkExt;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .setup(|app| {
            // A menu bar app: no Dock icon, lives in the tray.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            build_tray(app.handle())?;
            wire_deep_links(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it; shares and the inbox keep running.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::share,
            commands::stop_share,
            commands::receive,
            commands::inbox_open,
            commands::inbox_close,
            commands::inbox_decide,
            commands::send_to,
            commands::friends,
            commands::friend_add,
            commands::friend_remove,
            commands::identity,
            commands::identity_export,
            commands::doctor,
            commands::default_dir,
        ])
        .run(tauri::generate_context!())
        .expect("hither could not start");
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn toggle_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

/// The menu bar icon: left click toggles the window, right click menus.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open hither", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit hither", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &quit])?;
    TrayIconBuilder::with_id("hither")
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/tray.png"
        ))?)
        .icon_as_template(true)
        .tooltip("hither")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// `hither://<ticket>` from the landing page (or anywhere) lands here, both
/// at launch and while running.
fn wire_deep_links(app: &AppHandle) {
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            open_link(&handle, url.as_str());
        }
    });
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            open_link(app, url.as_str());
        }
    }
}

fn open_link(app: &AppHandle, url: &str) {
    use hither_core::link::{Link, parse_any};
    let payload = match parse_any(url) {
        Ok(Link::Share(t)) => serde_json::json!({ "kind": "share", "ticket": t.to_string() }),
        Ok(Link::Inbox(t)) => serde_json::json!({ "kind": "inbox", "ticket": t.to_string() }),
        Err(e) => serde_json::json!({ "kind": "invalid", "error": format!("{e:#}") }),
    };
    show_window(app);
    let _ = app.emit("hither-open", payload);
}
