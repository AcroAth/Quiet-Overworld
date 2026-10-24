//! Keeps Quiet Overworld playing while it is in the background on phones.
//!
//! - Android: a media-playback foreground service with a notification, so
//!   the system does not freeze or kill the app while music is on.
//! - iOS: the "playback" audio session, which together with the `audio`
//!   background mode in `Info.ios.plist` keeps the app running while the
//!   phone is locked. The music mixes with other apps instead of stopping
//!   them.

use std::sync::mpsc::{self, Sender};

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};

/// What the app is doing, as far as the OS needs to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Music is on: keep the app alive and show this in the notification.
    Active { title: String, text: String },
    /// Music is paused: the app may be suspended like any other.
    Inactive,
}

pub struct BackgroundAudio {
    tx: Sender<Notice>,
}

impl BackgroundAudio {
    /// Updates the background state. Never blocks; repeated states are
    /// ignored.
    pub fn set(&self, notice: Notice) {
        let _ = self.tx.send(notice);
    }
}

pub trait BackgroundAudioExt<R: Runtime> {
    /// The plugin state, once the plugin has been set up.
    fn background_audio(&self) -> Option<&BackgroundAudio>;
}

impl<R: Runtime, T: Manager<R>> BackgroundAudioExt<R> for T {
    fn background_audio(&self) -> Option<&BackgroundAudio> {
        self.try_state::<BackgroundAudio>().map(|state| state.inner())
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("background-audio")
        .setup(|app, api| {
            #[cfg(target_os = "ios")]
            ios::use_playback_session();

            #[cfg(target_os = "android")]
            let handle =
                api.register_android_plugin("io.github.acroath.quietoverworld.background", "BackgroundAudioPlugin")?;
            #[cfg(not(target_os = "android"))]
            let _ = api;

            let (tx, rx) = mpsc::channel::<Notice>();
            std::thread::Builder::new().name("background-audio".into()).spawn(move || {
                let mut last = None;
                while let Ok(mut notice) = rx.recv() {
                    // Only the newest state matters.
                    while let Ok(newer) = rx.try_recv() {
                        notice = newer;
                    }
                    if last.as_ref() == Some(&notice) {
                        continue;
                    }
                    #[cfg(target_os = "android")]
                    android::apply(&handle, &notice);
                    last = Some(notice);
                }
            })?;
            app.manage(BackgroundAudio { tx });
            Ok(())
        })
        .build()
}

#[cfg(target_os = "android")]
mod android {
    use serde::de::IgnoredAny;
    use serde::Serialize;
    use tauri::plugin::PluginHandle;
    use tauri::Runtime;

    use super::Notice;

    #[derive(Serialize)]
    struct StartArgs<'a> {
        title: &'a str,
        text: &'a str,
    }

    #[derive(Serialize)]
    struct NoArgs {}

    pub fn apply<R: Runtime>(handle: &PluginHandle<R>, notice: &Notice) {
        let result = match notice {
            Notice::Active { title, text } => {
                handle.run_mobile_plugin::<IgnoredAny>("start", StartArgs { title, text })
            }
            Notice::Inactive => handle.run_mobile_plugin::<IgnoredAny>("stop", NoArgs {}),
        };
        if let Err(error) = result {
            eprintln!("[quiet-overworld] background playback: {error}");
        }
    }
}

#[cfg(target_os = "ios")]
mod ios {
    use objc2_avf_audio::{AVAudioSession, AVAudioSessionCategoryOptions, AVAudioSessionCategoryPlayback};

    pub fn use_playback_session() {
        // SAFETY: plain calls on the shared audio session singleton.
        unsafe {
            let session = AVAudioSession::sharedInstance();
            if let Some(playback) = AVAudioSessionCategoryPlayback {
                if let Err(error) =
                    session.setCategory_withOptions_error(playback, AVAudioSessionCategoryOptions::MixWithOthers)
                {
                    eprintln!("[quiet-overworld] could not set the audio session category: {error:?}");
                }
            }
            if let Err(error) = session.setActive_error(true) {
                eprintln!("[quiet-overworld] could not activate the audio session: {error:?}");
            }
        }
    }
}
