mod audio;
mod catalog;
mod engine;
mod library;
mod settings;
mod sources;

#[cfg(mobile)]
mod mobile;
#[cfg(desktop)]
mod tray;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

use audio::OutputMode;
use engine::{Command, EngineHandle, Environment, Status};
use settings::Settings;
use sources::{LocalInstall, Mojang};

/// Command-line flag used by the autostart entry.
pub const HIDDEN_FLAG: &str = "--hidden";

pub struct AppState {
    engine: EngineHandle,
    settings: Mutex<Settings>,
    status: Arc<Mutex<Status>>,
    settings_path: PathBuf,
    install_roots: Vec<PathBuf>,
    mojang: Arc<Mojang>,
}

impl AppState {
    pub fn send(&self, command: Command) {
        self.engine.send(command);
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    settings: Settings,
    status: Status,
    mobile: bool,
    /// The game folder auto-detection would use, if any.
    detected_install: Option<String>,
    version: &'static str,
}

#[tauri::command]
fn snapshot(state: State<AppState>) -> Snapshot {
    Snapshot {
        settings: state.settings(),
        status: state.status.lock().unwrap().clone(),
        mobile: cfg!(mobile),
        detected_install: LocalInstall::find(&state.install_roots).map(|i| i.root().display().to_string()),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn update_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> Result<Settings, String> {
    let settings = settings.sanitized();
    #[cfg(desktop)]
    tray::set_autostart(&app, settings.autostart)?;
    settings.save(&state.settings_path)?;
    *state.settings.lock().unwrap() = settings.clone();
    state.send(Command::Settings(settings.clone()));
    let _ = app.emit("settings", &settings);
    Ok(settings)
}

#[tauri::command]
fn next_track(state: State<AppState>) {
    state.send(Command::Next);
}

#[tauri::command]
fn toggle_pause(state: State<AppState>) {
    state.send(Command::TogglePause);
}

#[tauri::command]
fn reload_library(state: State<AppState>) {
    state.send(Command::Reload);
}

/// Minecraft release versions, newest first.
#[tauri::command]
async fn list_versions(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let mojang = state.mojang.clone();
    tauri::async_runtime::spawn_blocking(move || mojang.releases()).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> Option<String> {
    #[cfg(desktop)]
    {
        tray::pick_folder(&app).await
    }
    #[cfg(mobile)]
    {
        let _ = app;
        None
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_window(app)))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_FLAG]),
        ))
        .plugin(tauri_plugin_dialog::init());
    #[cfg(mobile)]
    let builder = builder.plugin(mobile::init());

    let app = builder
        .invoke_handler(tauri::generate_handler![
            snapshot,
            update_settings,
            next_track,
            toggle_pause,
            reload_library,
            list_versions,
            pick_folder
        ])
        .setup(|app| {
            setup(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the app");

    app.run(|app, event| match event {
        // Closing the window only hides the app in the tray.
        RunEvent::ExitRequested { code: None, api, .. } => {
            let keep = app.try_state::<AppState>().is_some_and(|s| s.settings().close_to_tray);
            if keep && cfg!(desktop) {
                api.prevent_exit();
            }
        }
        #[cfg(desktop)]
        RunEvent::WindowEvent { label, event: tauri::WindowEvent::Destroyed, .. } => {
            if app.webview_windows().keys().all(|other| *other == label) {
                tray::set_dock_visible(app, false);
            }
        }
        RunEvent::Exit => {
            if let Some(state) = app.try_state::<AppState>() {
                state.send(Command::Shutdown);
            }
        }
        _ => {}
    });
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let paths = app.path();
    let settings_path = paths.app_config_dir()?.join("settings.json");
    let settings = Settings::load(&settings_path);
    let install_roots = LocalInstall::default_roots(paths.home_dir().ok().as_deref(), paths.data_dir().ok().as_deref());
    let mojang = Arc::new(
        match (std::env::var("QUIET_OVERWORLD_MANIFEST_URL"), std::env::var("QUIET_OVERWORLD_RESOURCES_URL")) {
            // For testing against a local mirror.
            (Ok(manifest), Ok(resources)) => Mojang::with_urls(&manifest, &resources),
            _ => Mojang::default(),
        },
    );
    let env = Environment {
        cache_file: paths.app_cache_dir()?.join("catalog.json"),
        default_install_roots: install_roots.clone(),
        mojang: mojang.clone(),
        output: if cfg!(mobile) { OutputMode::Phone } else { OutputMode::Desktop },
    };

    let status = Arc::new(Mutex::new(Status::default()));
    let observer = {
        let app = app.clone();
        let status = status.clone();
        move |new: &Status| {
            *status.lock().unwrap() = new.clone();
            let _ = app.emit("status", new);
            #[cfg(desktop)]
            tray::update(&app, new);
            #[cfg(mobile)]
            mobile::update(&app, new);
        }
    };
    let engine = EngineHandle::spawn(settings.clone(), env, observer);
    app.manage(AppState {
        engine,
        settings: Mutex::new(settings.clone()),
        status,
        settings_path,
        install_roots,
        mojang,
    });

    #[cfg(desktop)]
    {
        tray::create(app)?;
        let autostarted = std::env::args().any(|arg| arg == HIDDEN_FLAG);
        if !(settings.start_hidden || autostarted) {
            tray::show_window(app);
        } else {
            tray::set_dock_visible(app, false);
        }
    }
    Ok(())
}
