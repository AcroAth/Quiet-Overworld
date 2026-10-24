//! Desktop integration: tray icon, window lifecycle, autostart.
//!
//! Closing the window destroys the webview instead of hiding it, so the app
//! idles in the tray with only the small engine thread left.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewWindowBuilder, Wry};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::engine::{Command, State, Status};
use crate::AppState;

const MAIN: &str = "main";

struct TrayMenu {
    now: MenuItem<Wry>,
    pause: MenuItem<Wry>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let now = MenuItem::with_id(app, "now", "Looking for music…", false, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause", true, None::<&str>)?;
    let next = MenuItem::with_id(app, "next", "Play a song now", true, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Open Quiet Overworld", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(app, &[&now, &separator()?, &pause, &next, &separator()?, &show, &quit])?;

    let mut tray = TrayIconBuilder::with_id(MAIN)
        .tooltip("Quiet Overworld")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "pause" => send(app, Command::TogglePause),
            "next" => send(app, Command::Next),
            "show" => show_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    app.manage(TrayMenu { now, pause });
    Ok(())
}

fn send(app: &AppHandle, command: Command) {
    if let Some(state) = app.try_state::<AppState>() {
        state.send(command);
    }
}

/// Mirrors the engine status in the tray menu and tooltip.
pub fn update(app: &AppHandle, status: &Status) {
    let Some(menu) = app.try_state::<TrayMenu>() else { return };
    let line = describe(status);
    let _ = menu.now.set_text(&line);
    let _ = menu.pause.set_text(if status.paused { "Resume" } else { "Pause" });
    if let Some(tray) = app.tray_by_id(MAIN) {
        let _ = tray.set_tooltip(Some(format!("Quiet Overworld\n{line}")));
    }
}

fn describe(status: &Status) -> String {
    let track = match (&status.title, &status.artist) {
        (Some(title), Some(artist)) => format!("{title} — {artist}"),
        (Some(title), None) => title.clone(),
        _ => String::new(),
    };
    let text = match status.state {
        State::Starting => "Looking for music…".to_string(),
        State::Loading => format!("Loading {track}…"),
        State::Playing if status.paused => format!("Paused: {track}"),
        State::Playing => format!("♪ {track}"),
        State::Waiting if status.paused => "Paused".to_string(),
        State::Waiting => match &status.error {
            Some(error) => error.clone(),
            None => "Waiting for the next song".to_string(),
        },
    };
    // Menus and tooltips get cut off or wrap badly when long.
    if text.chars().count() > 60 {
        text.chars().take(59).chain(['…']).collect()
    } else {
        text
    }
}

static CREATING: AtomicBool = AtomicBool::new(false);

/// Shows the window, creating it if it was closed to the tray.
pub fn show_window(app: &AppHandle) {
    set_dock_visible(app, true);
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if CREATING.swap(true, Ordering::SeqCst) {
        return;
    }
    // Building a webview from an event handler can deadlock on Windows, so
    // do it from another thread.
    let app = app.clone();
    std::thread::spawn(move || {
        let config = app.config().app.windows.iter().find(|w| w.label == MAIN).cloned();
        if let Some(config) = config {
            match WebviewWindowBuilder::from_config(&app, &config).and_then(|builder| builder.build()) {
                Ok(window) => {
                    let _ = window.set_focus();
                }
                Err(error) => eprintln!("[quiet-overworld] could not open the window: {error}"),
            }
        }
        CREATING.store(false, Ordering::SeqCst);
    });
}

/// Hides the Dock icon on macOS while only the tray icon is left.
pub fn set_dock_visible(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        let policy = if visible { tauri::ActivationPolicy::Regular } else { tauri::ActivationPolicy::Accessory };
        let _ = app.set_activation_policy(policy);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

pub fn set_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    if launcher.is_enabled().unwrap_or(false) == enabled {
        return Ok(());
    }
    let result = if enabled { launcher.enable() } else { launcher.disable() };
    result.map_err(|e| format!("Could not change the start-up setting: {e}"))
}

pub async fn pick_folder(app: &AppHandle) -> Option<String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
        .await
        .ok()
        .flatten()
        .and_then(|folder| folder.into_path().ok())
        .map(|path| path.display().to_string())
}
