//! Audio output.
//!
//! The sound device is opened when a song starts and closed when it ends, so
//! the app holds no audio resources (and never keeps the computer awake)
//! during the long silences.

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rodio::mixer::Mixer;
use rodio::source::EmptyCallback;
use rodio::{CpalError, CpalErrorKind, Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

use crate::library::Audio;

/// A track decoded on the fly, ready to play.
pub struct Prepared {
    source: Box<dyn Source + Send>,
    pub duration: Option<Duration>,
}

impl Prepared {
    /// Skips ahead, used to resume a track after a pause. Returns false if
    /// the format cannot seek.
    pub fn seek(&mut self, position: Duration) -> bool {
        self.source.try_seek(position).is_ok()
    }
}

/// Probes the audio format.
pub fn prepare(audio: &Audio, hint: Option<&str>) -> Result<Prepared, String> {
    let undecodable = |e: rodio::decoder::DecoderError| format!("Could not decode the track: {e}");
    let source: Box<dyn Source + Send> = match audio {
        Audio::Memory(bytes) => {
            let len = bytes.len() as u64;
            let mut builder = Decoder::builder().with_data(Cursor::new(bytes.clone())).with_byte_len(len);
            if let Some(hint) = hint {
                builder = builder.with_hint(hint);
            }
            Box::new(builder.build().map_err(undecodable)?)
        }
        Audio::File(path) => {
            let file = File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
            let len = file.metadata().map(|m| m.len()).map_err(|e| e.to_string())?;
            let mut builder = Decoder::builder().with_data(BufReader::new(file)).with_byte_len(len);
            if let Some(hint) = hint {
                builder = builder.with_hint(hint);
            }
            Box::new(builder.build().map_err(undecodable)?)
        }
    };
    let duration = source.total_duration().filter(|d| !d.is_zero());
    Ok(Prepared { source, duration })
}

/// How the engine uses the sound device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    /// Open the device only while a song plays.
    Desktop,
    /// No device: a thread consumes the audio about 100 times faster than
    /// real time. Used by tests.
    #[allow(dead_code)]
    Headless,
}

pub struct Output {
    sink: Option<Sink>,
    mode: OutputMode,
    /// Runs on the audio thread if the device goes away.
    on_lost: Arc<dyn Fn() + Send + Sync>,
}

impl Output {
    pub fn new(mode: OutputMode, on_lost: impl Fn() + Send + Sync + 'static) -> Output {
        Output { sink: None, mode, on_lost: Arc::new(on_lost) }
    }

    /// Starts a track. `on_end` runs on the audio thread once the track has
    /// played to its end.
    pub fn play(
        &mut self,
        prepared: Prepared,
        volume: f32,
        on_end: impl Fn() + Send + 'static,
    ) -> Result<Player, String> {
        if self.sink.is_none() {
            self.sink = Some(self.open()?);
        }
        let sink = self.sink.as_ref().expect("opened above");
        let player = Player::connect_new(sink.mixer());
        player.set_volume(volume);
        player.append(prepared.source);
        player.append(EmptyCallback::new(Box::new(on_end)));
        Ok(player)
    }

    /// Called between songs: releases the device.
    pub fn idle(&mut self) {
        self.sink = None;
    }

    /// Drops the device, e.g. while paused or after it reported an error.
    pub fn close(&mut self) {
        self.sink = None;
    }

    fn open(&self) -> Result<Sink, String> {
        match self.mode {
            OutputMode::Headless => Ok(Sink::headless()),
            _ => open_device(self.on_lost.clone()).map(Sink::Device),
        }
    }
}

enum Sink {
    Device(MixerDeviceSink),
    Headless(Mixer, Arc<AtomicBool>),
}

impl Sink {
    fn mixer(&self) -> &Mixer {
        match self {
            Sink::Device(sink) => sink.mixer(),
            Sink::Headless(mixer, _) => mixer,
        }
    }

    fn headless() -> Sink {
        let (mixer, mut samples) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                samples.by_ref().take(9_600).for_each(drop);
                thread::sleep(Duration::from_millis(1));
            }
        });
        Sink::Headless(mixer, stop)
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        if let Sink::Headless(_, stop) = self {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

fn open_device(on_lost: Arc<dyn Fn() + Send + Sync>) -> Result<MixerDeviceSink, String> {
    // Report a lost device once per stream; glitches and reroutes are harmless.
    let reported = Arc::new(AtomicBool::new(false));
    let callback = move |error: CpalError| match error.kind() {
        CpalErrorKind::Xrun
        | CpalErrorKind::DeviceChanged
        | CpalErrorKind::RealtimeDenied
        | CpalErrorKind::DeviceBusy => {}
        _ => {
            if !reported.swap(true, Ordering::SeqCst) {
                eprintln!("[quiet-overworld] audio device error: {error}");
                on_lost();
            }
        }
    };
    let mut sink = DeviceSinkBuilder::from_default_device()
        .and_then(|builder| builder.with_error_callback(callback).open_sink_or_fallback())
        .or_else(|_| DeviceSinkBuilder::open_default_sink())
        .map_err(|e| format!("No audio output available: {e}"))?;
    sink.log_on_drop(false);
    Ok(sink)
}
