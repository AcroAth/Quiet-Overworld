//! The music engine: the game's `MusicManager` timing, driven by deadlines
//! instead of twenty ticks per second, so the thread sleeps between songs.
//!
//! The game's rules, which this follows:
//! - the first song starts 5 seconds after launch;
//! - after a song ends, the next one waits a random delay picked from the
//!   current music's range (10–20 minutes in the overworld), adjusted by the
//!   "Music Frequency" option;
//! - switching to music that replaces the current song (title screen, the
//!   End) stops the song and starts the new music within half its minimum
//!   delay; switching to any other music lets the current song finish;
//! - a pending wait is cut down to the new music's longest delay.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::audio::{self, Output, OutputMode, Prepared};
use crate::library::{install_roots, Audio, Choice, Library, LibraryRequest, Origin, REFRESH_AFTER};
use crate::settings::{Settings, STARTING_DELAY, TICK};
use crate::sources::Mojang;

/// How often a playing song is checked for a stalled audio device.
const WATCHDOG: Duration = Duration::from_secs(30);

pub enum Command {
    /// Play another track now.
    Next,
    SetPaused(bool),
    TogglePause,
    Settings(Settings),
    /// Look for music again, e.g. after installing the game.
    Reload,
    Shutdown,
    LibraryLoaded(u64, Result<Library, String>),
    TrackLoaded(u64, Result<Loaded, String>),
    TrackEnded(u64),
    DeviceLost,
}

/// A fetched track, sent from a loader thread.
pub struct Loaded {
    prepared: Prepared,
    audio: Audio,
    origin: Origin,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Looking for music for the first time.
    #[default]
    Starting,
    /// Silence between songs.
    Waiting,
    /// Fetching a track.
    Loading,
    Playing,
}

/// What the UI and tray show.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: State,
    pub paused: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub origin: Option<Origin>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    /// Silence left before the next song, if known.
    pub next_in_ms: Option<u64>,
    /// Shortest and longest silence between songs with the current settings.
    pub gap_ms: (u64, u64),
    /// When the numbers above were taken, in ms since the Unix epoch.
    pub at_ms: u64,
    /// E.g. "Minecraft 26.3 · 33 tracks".
    pub library: Option<String>,
    /// Music situations the library has, e.g. "music.game".
    pub situations: Vec<String>,
    pub error: Option<String>,
}

/// Platform facts the engine needs.
pub struct Environment {
    pub cache_file: PathBuf,
    pub default_install_roots: Vec<PathBuf>,
    pub mojang: Arc<Mojang>,
    pub output: OutputMode,
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Command>,
}

impl EngineHandle {
    pub fn spawn(settings: Settings, env: Environment, observer: impl FnMut(&Status) + Send + 'static) -> EngineHandle {
        let (tx, rx) = mpsc::channel();
        let engine_tx = tx.clone();
        thread::Builder::new()
            .name("music-engine".into())
            .spawn(move || Engine::new(settings, env, engine_tx, Box::new(observer)).run(rx))
            .expect("failed to start the music engine");
        EngineHandle { tx }
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

struct Track {
    gen: u64,
    key: String,
    title: String,
    artist: Option<String>,
    origin: Origin,
    /// The game's per-track volume.
    gain: f32,
    audio: Audio,
    hint: Option<String>,
    duration: Option<Duration>,
}

enum Phase {
    /// Silence. `due` is when the next song starts. It is None while paused
    /// (`left` then holds the remaining time) and when nothing can play.
    Waiting {
        due: Option<Instant>,
        left: Duration,
    },
    Loading {
        gen: u64,
        key: String,
        title: String,
        artist: Option<String>,
        hint: Option<String>,
        gain: f32,
    },
    Playing {
        track: Track,
        player: rodio::Player,
        offset: Duration,
        check_at: Instant,
        last_pos: Duration,
    },
    /// A song paused part-way. The audio device is released meanwhile.
    Paused {
        track: Track,
        position: Duration,
    },
}

struct Engine {
    tx: Sender<Command>,
    observer: Box<dyn FnMut(&Status) + Send>,
    env: Environment,
    settings: Settings,
    rng: fastrand::Rng,
    paused: bool,
    phase: Phase,
    output: Output,
    library: Option<Arc<Library>>,
    library_gen: u64,
    library_error: Option<String>,
    library_failures: u32,
    /// When to retry or refresh the library.
    library_due: Option<Instant>,
    track_gen: u64,
    track_error: Option<String>,
    track_failures: u32,
    device_lost_at: Option<Instant>,
    published: Option<Status>,
}

impl Engine {
    fn new(
        settings: Settings,
        env: Environment,
        tx: Sender<Command>,
        observer: Box<dyn FnMut(&Status) + Send>,
    ) -> Engine {
        let lost_tx = tx.clone();
        let output = Output::new(env.output, move || {
            let _ = lost_tx.send(Command::DeviceLost);
        });
        Engine {
            tx,
            observer,
            env,
            settings,
            rng: fastrand::Rng::new(),
            paused: false,
            phase: Phase::Waiting { due: None, left: Duration::ZERO },
            output,
            library: None,
            library_gen: 0,
            library_error: None,
            library_failures: 0,
            library_due: None,
            track_gen: 0,
            track_error: None,
            track_failures: 0,
            device_lost_at: None,
            published: None,
        }
    }

    fn run(mut self, rx: Receiver<Command>) {
        self.load_library();
        self.wait(STARTING_DELAY);
        self.publish();
        loop {
            let command = match self.deadline() {
                Some(deadline) => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(command) => Some(command),
                    Err(_) => break,
                },
            };
            match command {
                Some(Command::Shutdown) => break,
                Some(command) => self.handle(command),
                None => self.on_deadline(),
            }
            self.publish();
        }
        self.phase = Phase::Waiting { due: None, left: Duration::ZERO };
        self.output.close();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Next => {
                let avoid = self.current_key();
                self.paused = false;
                self.phase = Phase::Waiting { due: None, left: Duration::ZERO };
                self.start_track(avoid);
            }
            Command::SetPaused(paused) => self.set_paused(paused),
            Command::TogglePause => self.set_paused(!self.paused),
            Command::Settings(settings) => self.apply_settings(settings.sanitized()),
            Command::Reload => {
                self.library = None;
                self.load_library();
            }
            Command::Shutdown => {}
            Command::LibraryLoaded(gen, result) => self.library_loaded(gen, result),
            Command::TrackLoaded(gen, result) => self.track_loaded(gen, result),
            Command::TrackEnded(gen) => {
                if matches!(&self.phase, Phase::Playing { track, .. } if track.gen == gen) {
                    self.finish_track();
                }
            }
            Command::DeviceLost => {
                self.output.close();
                // A device that keeps failing is left closed until the next song.
                let recent = self.device_lost_at.is_some_and(|at| at.elapsed() < Duration::from_secs(10));
                self.device_lost_at = Some(Instant::now());
                if matches!(self.phase, Phase::Playing { .. }) {
                    self.finish_track();
                } else if matches!(self.phase, Phase::Waiting { .. }) && !self.paused && !recent {
                    self.output.idle();
                }
            }
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let phase = match &self.phase {
            Phase::Waiting { due: Some(due), .. } if self.library.is_some() => Some(*due),
            Phase::Playing { check_at, .. } => Some(*check_at),
            _ => None,
        };
        match (phase, self.library_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn on_deadline(&mut self) {
        let now = Instant::now();
        if self.library_due.is_some_and(|due| due <= now) {
            self.library_due = None;
            self.load_library();
        }
        let mut stalled = false;
        match &mut self.phase {
            Phase::Waiting { due: Some(due), .. } if *due <= now && self.library.is_some() => {
                self.start_track(None);
            }
            Phase::Playing { player, check_at, last_pos, .. } if *check_at <= now => {
                let position = player.get_pos();
                if player.empty() || position == *last_pos {
                    // The end callback never came or the device stopped pulling audio.
                    stalled = !player.empty();
                    self.finish_track();
                } else {
                    *last_pos = position;
                    *check_at = now + WATCHDOG;
                }
            }
            _ => {}
        }
        if stalled {
            eprintln!("[quiet-overworld] audio output stalled, reopening it for the next song");
            self.output.close();
        }
    }

    fn current_key(&self) -> Option<String> {
        match &self.phase {
            Phase::Playing { track, .. } | Phase::Paused { track, .. } => Some(track.key.clone()),
            Phase::Loading { key, .. } => Some(key.clone()),
            Phase::Waiting { .. } => None,
        }
    }

    /// Enters the silence before the next song.
    fn wait(&mut self, delay: Duration) {
        let due = (!self.paused).then(|| Instant::now() + delay);
        self.phase = Phase::Waiting { due, left: delay };
    }

    fn start_track(&mut self, avoid: Option<String>) {
        let Some(library) = self.library.clone() else {
            // Starts as soon as the library is ready.
            self.wait(Duration::ZERO);
            return;
        };
        let Some(choice) = library.pick(&self.settings.situation, &mut self.rng, avoid.as_deref()) else {
            self.track_error = Some("There is no music for this situation here, pick another one".into());
            self.phase = Phase::Waiting { due: None, left: Duration::ZERO };
            self.output.idle();
            return;
        };
        self.track_gen += 1;
        let gen = self.track_gen;
        let hint = choice.format_hint();
        self.phase = Phase::Loading {
            gen,
            key: choice.key.clone(),
            title: choice.title.clone(),
            artist: choice.artist.clone(),
            hint: hint.clone(),
            gain: choice.volume,
        };
        let tx = self.tx.clone();
        let spawned = thread::Builder::new().name("track-loader".into()).spawn(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(|| load(&choice, hint.as_deref())))
                .unwrap_or_else(|_| Err(format!("\"{}\" could not be read", choice.title)));
            let _ = tx.send(Command::TrackLoaded(gen, result));
        });
        if let Err(e) = spawned {
            self.fail_track(format!("Could not load the track: {e}"));
        }
    }

    fn track_loaded(&mut self, gen: u64, result: Result<Loaded, String>) {
        if !matches!(&self.phase, Phase::Loading { gen: current, .. } if *current == gen) {
            return; // A newer request replaced this one.
        }
        let Phase::Loading { key, title, artist, hint, gain, .. } =
            std::mem::replace(&mut self.phase, Phase::Waiting { due: None, left: Duration::ZERO })
        else {
            return;
        };
        let loaded = match result {
            Ok(loaded) => loaded,
            Err(error) => return self.fail_track(error),
        };
        let track = Track {
            gen,
            key,
            title,
            artist,
            origin: loaded.origin,
            gain,
            audio: loaded.audio,
            hint,
            duration: loaded.prepared.duration,
        };
        if self.paused {
            self.output.idle();
            self.phase = Phase::Paused { track, position: Duration::ZERO };
        } else {
            self.play(track, loaded.prepared, Duration::ZERO);
        }
    }

    fn play(&mut self, track: Track, prepared: Prepared, offset: Duration) {
        let gen = track.gen;
        let end_tx = self.tx.clone();
        let played = self.output.play(prepared, self.settings.volume * track.gain, move || {
            let _ = end_tx.send(Command::TrackEnded(gen));
        });
        match played {
            Ok(player) => {
                self.track_error = None;
                self.track_failures = 0;
                let check_at = Instant::now() + WATCHDOG;
                self.phase = Phase::Playing { track, player, offset, check_at, last_pos: Duration::ZERO };
            }
            Err(error) => self.fail_track(error),
        }
    }

    fn fail_track(&mut self, error: String) {
        eprintln!("[quiet-overworld] {error}");
        self.track_failures += 1;
        let retry = Duration::from_secs(15) * 2u32.pow(self.track_failures.min(6) - 1);
        self.track_error = Some(error);
        self.output.idle();
        self.wait(retry.min(Duration::from_secs(600)));
    }

    fn finish_track(&mut self) {
        self.phase = Phase::Waiting { due: None, left: Duration::ZERO };
        self.output.idle();
        let delay = self.settings.next_delay(&mut self.rng);
        self.wait(delay);
    }

    fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        let now = Instant::now();
        let phase = std::mem::replace(&mut self.phase, Phase::Waiting { due: None, left: Duration::ZERO });
        self.phase = match phase {
            Phase::Playing { track, player, offset, .. } if paused => {
                let position = offset + player.get_pos();
                drop(player);
                Phase::Paused { track, position }
            }
            Phase::Paused { track, position } if !paused => {
                match audio::prepare(&track.audio, track.hint.as_deref()) {
                    Ok(mut prepared) => {
                        let offset = if prepared.seek(position) { position } else { Duration::ZERO };
                        self.play(track, prepared, offset);
                    }
                    Err(error) => self.fail_track(error),
                }
                return;
            }
            Phase::Waiting { due, left } if paused => {
                let left = due.map_or(left, |due| due.saturating_duration_since(now));
                Phase::Waiting { due: None, left }
            }
            Phase::Waiting { due: None, left } if !paused => Phase::Waiting { due: Some(now + left), left },
            other => other,
        };
        if paused {
            // Release the device: even a silent stream keeps it busy and can
            // keep a computer from sleeping.
            self.output.close();
        } else if matches!(self.phase, Phase::Waiting { .. }) {
            self.output.idle();
        }
    }

    fn apply_settings(&mut self, new: Settings) {
        let old = std::mem::replace(&mut self.settings, new);
        if let Phase::Playing { track, player, .. } = &self.phase {
            player.set_volume(self.settings.volume * track.gain);
        }
        if !self.settings.same_library(&old) {
            // The current song keeps playing; the next one comes from the new library.
            self.library = None;
            self.library_error = None;
            self.track_error = None;
            self.load_library();
        }
        let timing_changed = old.situation != self.settings.situation
            || old.frequency != self.settings.frequency
            || old.custom_min_minutes != self.settings.custom_min_minutes
            || old.custom_max_minutes != self.settings.custom_max_minutes;
        if !timing_changed {
            return;
        }
        self.track_error = None;
        let timing = self.settings.timing();
        let replaces = old.situation != self.settings.situation && timing.replace_current;
        let max = self.settings.max_delay();
        let now = Instant::now();
        match &mut self.phase {
            Phase::Playing { .. } | Phase::Paused { .. } | Phase::Loading { .. } if replaces => {
                let delay = TICK * self.rng.u32(0..=timing.min_delay / 2);
                self.output.idle();
                self.wait(delay);
            }
            Phase::Waiting { due: Some(due), .. } => *due = (*due).min(now + max),
            Phase::Waiting { due, left } => {
                *left = (*left).min(max);
                if !self.paused {
                    // Nothing could play before; try the new music.
                    *due = Some(now + *left);
                }
            }
            _ => {}
        }
    }

    fn load_library(&mut self) {
        self.library_gen += 1;
        let gen = self.library_gen;
        let request = LibraryRequest {
            source: self.settings.source,
            game_version: self.settings.game_version.clone(),
            install_roots: install_roots(&self.settings, &self.env.default_install_roots),
            music_folder: self.settings.music_folder.clone(),
            cache_file: self.env.cache_file.clone(),
            mojang: self.env.mojang.clone(),
        };
        let tx = self.tx.clone();
        let spawned = thread::Builder::new().name("library-loader".into()).spawn(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(|| request.load()))
                .unwrap_or_else(|_| Err("Could not read the music library".into()));
            let _ = tx.send(Command::LibraryLoaded(gen, result));
        });
        if spawned.is_err() {
            self.library_due = Some(Instant::now() + Duration::from_secs(60));
        }
    }

    fn library_loaded(&mut self, gen: u64, result: Result<Library, String>) {
        if gen != self.library_gen {
            return;
        }
        match result {
            Ok(library) => {
                self.library_due = library.wants_refresh(&self.settings).then(|| Instant::now() + REFRESH_AFTER);
                self.library = Some(Arc::new(library));
                self.library_error = None;
                self.library_failures = 0;
                if let Phase::Waiting { due: due @ None, left } = &mut self.phase {
                    if !self.paused {
                        *due = Some(Instant::now() + *left);
                    }
                }
            }
            Err(error) => {
                eprintln!("[quiet-overworld] {error}");
                self.library_failures += 1;
                let retry = Duration::from_secs(30) * 2u32.pow(self.library_failures.min(6) - 1);
                self.library_due = Some(Instant::now() + retry.min(Duration::from_secs(30 * 60)));
                // Keep playing from the old library if a refresh failed.
                if self.library.is_none() {
                    self.library_error = Some(error);
                }
            }
        }
    }

    fn status(&self) -> Status {
        let now = Instant::now();
        let mut status = Status {
            paused: self.paused,
            at_ms: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            library: self.library.as_ref().map(|l| l.describe(&self.settings.situation)),
            situations: self.library.as_ref().map(|l| l.situations()).unwrap_or_default(),
            error: self.track_error.clone().or_else(|| self.library_error.clone()),
            ..Status::default()
        };
        let ms = |d: Duration| d.as_millis() as u64;
        let (min_gap, max_gap) = self.settings.delay_range();
        status.gap_ms = (ms(min_gap), ms(max_gap));
        let mut show = |track: &Track, position: Duration| {
            status.title = Some(track.title.clone());
            status.artist = track.artist.clone();
            status.origin = Some(track.origin);
            status.position_ms = ms(position);
            status.duration_ms = track.duration.map(ms);
        };
        match &self.phase {
            Phase::Playing { track, player, offset, .. } => {
                show(track, *offset + player.get_pos());
                status.state = State::Playing;
            }
            Phase::Paused { track, position } => {
                show(track, *position);
                status.state = State::Playing;
            }
            Phase::Loading { title, artist, .. } => {
                status.state = State::Loading;
                status.title = Some(title.clone());
                status.artist = artist.clone();
            }
            Phase::Waiting { due, left } => {
                let starting = self.library.is_none() && self.library_error.is_none();
                status.state = if starting { State::Starting } else { State::Waiting };
                let blocked = due.is_none() && !self.paused;
                if !starting && !blocked {
                    status.next_in_ms = Some(ms(due.map_or(*left, |due| due.saturating_duration_since(now))));
                }
            }
        }
        status
    }

    /// Tells the observer about changes. Positions and timestamps alone do not
    /// count as changes because the UI extrapolates them.
    fn publish(&mut self) {
        let status = self.status();
        let comparable =
            |s: &Status| Status { at_ms: 0, position_ms: 0, next_in_ms: s.next_in_ms.map(|ms| ms / 1000), ..s.clone() };
        if self.published.as_ref().map(comparable) != Some(comparable(&status)) {
            (self.observer)(&status);
            self.published = Some(status);
        }
    }
}

/// Runs on a loader thread: fetch the audio and probe its format.
fn load(choice: &Choice, hint: Option<&str>) -> Result<Loaded, String> {
    let (audio, origin) = choice.load()?;
    let prepared = audio::prepare(&audio, hint)?;
    Ok(Loaded { prepared, audio, origin })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::settings::{Frequency, SourceMode};

    /// Writes a silent mono WAV file.
    fn write_wav(path: &Path, seconds: f32) {
        let rate = 8_000u32;
        let data = (rate as f32 * seconds) as u32 * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        for field in [16u32.to_le_bytes(), [1, 0, 1, 0], rate.to_le_bytes(), (rate * 2).to_le_bytes(), [2, 0, 16, 0]] {
            bytes.extend_from_slice(&field);
        }
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data.to_le_bytes());
        bytes.resize(bytes.len() + data as usize, 0);
        fs::write(path, bytes).unwrap();
    }

    /// Runs the engine on a folder of two silent tracks with a headless
    /// output that plays about 100 times faster than real time.
    struct Harness {
        engine: EngineHandle,
        statuses: Receiver<Status>,
        dir: PathBuf,
    }

    impl Harness {
        fn start(name: &str, track_seconds: f32, frequency: (f64, f64)) -> Harness {
            let dir = std::env::temp_dir().join(format!("quiet-overworld-engine-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            write_wav(&dir.join("C418 - Sweden.wav"), track_seconds);
            write_wav(&dir.join("C418 - Wet Hands.wav"), track_seconds);
            let settings = Settings {
                source: SourceMode::Folder,
                music_folder: dir.display().to_string(),
                frequency: Frequency::Custom,
                custom_min_minutes: frequency.0,
                custom_max_minutes: frequency.1,
                ..Settings::default()
            };
            let env = Environment {
                cache_file: dir.join("catalog.json"),
                default_install_roots: Vec::new(),
                mojang: Arc::new(Mojang::default()),
                output: OutputMode::Headless,
            };
            let (tx, statuses) = mpsc::channel();
            let engine = EngineHandle::spawn(settings, env, move |status: &Status| {
                let _ = tx.send(status.clone());
            });
            Harness { engine, statuses, dir }
        }

        /// Waits for a status matching `wanted`.
        fn until(&self, what: &str, wanted: impl Fn(&Status) -> bool) -> Status {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match self.statuses.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(status) if wanted(&status) => return status,
                    Ok(_) => {}
                    Err(_) => panic!("timed out waiting for {what}"),
                }
            }
        }

        fn playing(&self) -> Status {
            self.until("a song", |s| s.state == State::Playing && !s.paused)
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.engine.send(Command::Shutdown);
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn first_song_starts_after_the_games_starting_delay() {
        let h = Harness::start("start", 1.0, (10.0, 20.0));
        let status = h.until("the first wait", |s| s.state == State::Waiting);
        let left = status.next_in_ms.expect("a countdown");
        assert!((4_000..=5_000).contains(&left), "first song in {left} ms");
        assert_eq!(status.library.as_deref(), Some("Your music · 2 files"));
    }

    #[test]
    fn songs_are_separated_by_the_frequency_range() {
        let h = Harness::start("gaps", 1.0, (0.01, 0.02));
        h.engine.send(Command::Next);
        let song = h.playing();
        assert_eq!(song.artist.as_deref(), Some("C418"));
        assert!(matches!(song.title.as_deref(), Some("Sweden" | "Wet Hands")));
        let gap = h.until("the gap after the song", |s| s.state == State::Waiting).next_in_ms.unwrap();
        assert!((500..=1_200).contains(&gap), "gap of {gap} ms");
        assert_eq!(h.until("the gap range", |_| true).gap_ms, (600, 1_200));
        h.playing();
    }

    #[test]
    fn next_plays_another_track_right_away() {
        let h = Harness::start("next", 30.0, (10.0, 20.0));
        h.engine.send(Command::Next);
        let first = h.playing().title;
        h.engine.send(Command::Next);
        let second = h.until("another song", |s| s.state == State::Playing && s.title != first);
        assert!(second.title.is_some());
    }

    #[test]
    fn pausing_freezes_the_countdown() {
        let h = Harness::start("pause-wait", 0.2, (1.0, 1.0));
        h.engine.send(Command::Next);
        h.playing();
        h.until("the gap", |s| s.state == State::Waiting && !s.paused);
        h.engine.send(Command::SetPaused(true));
        let paused = h.until("pause", |s| s.paused).next_in_ms.unwrap();
        thread::sleep(Duration::from_millis(300));
        h.engine.send(Command::SetPaused(false));
        let resumed = h.until("resume", |s| !s.paused && s.state == State::Waiting).next_in_ms.unwrap();
        assert!(paused > 59_000 && resumed > 59_000, "paused at {paused} ms, resumed at {resumed} ms");
        assert!(paused.abs_diff(resumed) < 250, "the countdown moved while paused");
    }

    #[test]
    fn pausing_a_song_resumes_where_it_stopped() {
        let h = Harness::start("pause-song", 60.0, (10.0, 20.0));
        h.engine.send(Command::Next);
        let title = h.playing().title;
        thread::sleep(Duration::from_millis(100));
        h.engine.send(Command::SetPaused(true));
        let paused = h.until("pause", |s| s.paused && s.state == State::Playing);
        assert!(paused.position_ms > 0);
        h.engine.send(Command::SetPaused(false));
        let resumed = h.playing();
        assert_eq!(resumed.title, title);
        assert!(resumed.position_ms >= paused.position_ms, "restarted from the beginning");
    }
}
