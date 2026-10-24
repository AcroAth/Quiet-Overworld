//! Phone integration: keeps the app alive in the background while music is on.

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Runtime};
use tauri_plugin_background_audio::{BackgroundAudioExt, Notice};

use crate::engine::{State, Status};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    tauri_plugin_background_audio::init()
}

pub fn update(app: &AppHandle, status: &Status) {
    let notice = if status.paused {
        Notice::Inactive
    } else {
        let text = match (status.state, &status.title, &status.artist) {
            (State::Playing, Some(title), Some(artist)) => format!("♪ {title} — {artist}"),
            (State::Playing, Some(title), None) => format!("♪ {title}"),
            (State::Starting | State::Loading, ..) => "Getting the next song…".to_string(),
            _ => "Waiting for the next song".to_string(),
        };
        Notice::Active { title: "Quiet Overworld".into(), text }
    };
    if let Some(background) = app.background_audio() {
        background.set(notice);
    }
}
