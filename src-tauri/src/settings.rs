//! User settings and the game's music timing rules.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The overworld music played to a survival player in most biomes.
pub const DEFAULT_SITUATION: &str = "music.game";

/// One game tick. The game measures music delays in ticks.
pub const TICK: Duration = Duration::from_millis(50);

/// `MusicManager` waits 100 ticks before its first song.
pub const STARTING_DELAY: Duration = Duration::from_secs(5);

/// The game's "Music Frequency" option, plus a custom range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frequency {
    /// Normal delays: 10–20 minutes between overworld songs.
    #[default]
    Default,
    /// Delays capped at 10 minutes.
    Frequent,
    /// 5 seconds between songs.
    Constant,
    /// The user's own range.
    Custom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceMode {
    /// Installed game files when present, otherwise streamed from Mojang.
    #[default]
    Minecraft,
    /// Installed game files only, never uses the network.
    Local,
    /// A folder of the user's own music.
    Folder,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Sound event to play, e.g. "music.game" or "music.overworld.meadow".
    pub situation: String,
    pub frequency: Frequency,
    pub custom_min_minutes: f64,
    pub custom_max_minutes: f64,
    /// Music volume, 0 to 1, like the game's slider.
    pub volume: f32,
    pub source: SourceMode,
    /// Game version to take music from; empty means the latest release.
    pub game_version: String,
    /// Game data folder; empty means auto-detect.
    pub minecraft_dir: String,
    pub music_folder: String,
    pub close_to_tray: bool,
    pub start_hidden: bool,
    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            situation: DEFAULT_SITUATION.into(),
            frequency: Frequency::Default,
            custom_min_minutes: 3.0,
            custom_max_minutes: 8.0,
            volume: 1.0,
            source: SourceMode::Minecraft,
            game_version: String::new(),
            minecraft_dir: String::new(),
            music_folder: String::new(),
            close_to_tray: true,
            start_hidden: false,
            autostart: false,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).and_then(|_| fs::rename(&tmp, path)).map_err(|e| e.to_string())
    }

    /// Clamps values coming from disk or the UI into sane ranges.
    pub fn sanitized(mut self) -> Settings {
        if self.situation.trim().is_empty() {
            self.situation = DEFAULT_SITUATION.into();
        }
        self.volume = if self.volume.is_finite() { self.volume.clamp(0.0, 1.0) } else { 1.0 };
        let minutes = |m: f64, fallback: f64| if m.is_finite() { m.clamp(0.0, 24.0 * 60.0) } else { fallback };
        self.custom_min_minutes = minutes(self.custom_min_minutes, 3.0);
        self.custom_max_minutes = minutes(self.custom_max_minutes, 8.0).max(self.custom_min_minutes);
        self.game_version = self.game_version.trim().to_string();
        self.minecraft_dir = self.minecraft_dir.trim().to_string();
        self.music_folder = self.music_folder.trim().to_string();
        self
    }

    /// Whether switching from `other` requires loading a different library.
    pub fn same_library(&self, other: &Settings) -> bool {
        self.source == other.source
            && self.game_version == other.game_version
            && self.minecraft_dir == other.minecraft_dir
            && self.music_folder == other.music_folder
    }

    /// The silence after a song ends, like `MusicFrequency.getNextSongDelay`.
    pub fn next_delay(&self, rng: &mut fastrand::Rng) -> Duration {
        let (min, max) = self.delay_range();
        if max <= min {
            return min;
        }
        let ms = rng.u64(min.as_millis() as u64..=max.as_millis() as u64);
        Duration::from_millis(ms)
    }

    /// The longest possible silence. A pending wait is cut down to this when
    /// the situation or frequency changes, as the game does every tick.
    pub fn max_delay(&self) -> Duration {
        self.delay_range().1
    }

    /// The timing rules in effect. A music folder has no situations and
    /// uses the overworld's.
    pub fn timing(&self) -> Timing {
        match self.source {
            SourceMode::Folder => timing(DEFAULT_SITUATION),
            _ => timing(&self.situation),
        }
    }

    /// Shortest and longest silence between songs.
    pub fn delay_range(&self) -> (Duration, Duration) {
        let timing = self.timing();
        let capped = |cap: u32| (TICK * timing.min_delay.min(cap), TICK * timing.max_delay.min(cap));
        match self.frequency {
            Frequency::Default => capped(24_000),
            Frequency::Frequent => capped(12_000),
            Frequency::Constant => (TICK * 100, TICK * 100),
            Frequency::Custom => (
                Duration::from_secs_f64(self.custom_min_minutes * 60.0),
                Duration::from_secs_f64(self.custom_max_minutes * 60.0),
            ),
        }
    }
}

/// Delays of one kind of music in game ticks, from the game's `Musics` class
/// and the `background_music` attributes of its dimensions and biomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub min_delay: u32,
    pub max_delay: u32,
    /// Whether switching to this music stops the song that is playing.
    pub replace_current: bool,
}

pub fn timing(situation: &str) -> Timing {
    let (min_delay, max_delay, replace_current) = match situation {
        "music.menu" => (20, 600, true),
        "music.end" | "music.game.end" => (6_000, 24_000, true),
        "music.dragon" | "music.game.end.dragon" => (0, 0, true),
        "music.credits" | "music.game.end.credits" => (0, 0, true),
        // Overworld, creative, underwater, nether and biome music.
        _ => (12_000, 24_000, false),
    };
    Timing { min_delay, max_delay, replace_current }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(situation: &str, frequency: Frequency) -> Settings {
        Settings { situation: situation.into(), frequency, ..Settings::default() }
    }

    #[test]
    fn overworld_waits_ten_to_twenty_minutes_by_default() {
        let s = Settings::default();
        let mut rng = fastrand::Rng::with_seed(3);
        let delays: Vec<Duration> = (0..2_000).map(|_| s.next_delay(&mut rng)).collect();
        assert!(delays.iter().all(|d| *d >= Duration::from_secs(600) && *d <= Duration::from_secs(1200)));
        assert!(delays.iter().any(|d| *d < Duration::from_secs(660)));
        assert!(delays.iter().any(|d| *d > Duration::from_secs(1140)));
        assert_eq!(s.max_delay(), Duration::from_secs(1200));
    }

    #[test]
    fn frequency_option_matches_the_game() {
        let mut rng = fastrand::Rng::with_seed(9);
        assert_eq!(with("music.game", Frequency::Frequent).next_delay(&mut rng), Duration::from_secs(600));
        assert_eq!(with("music.game", Frequency::Constant).next_delay(&mut rng), Duration::from_secs(5));
        let end = with("music.end", Frequency::Frequent);
        assert_eq!(end.max_delay(), Duration::from_secs(600));
        assert!(end.next_delay(&mut rng) >= Duration::from_secs(300));
        let menu = with("music.menu", Frequency::Default);
        assert!(menu.next_delay(&mut rng) <= Duration::from_secs(30));
        assert_eq!(with("music.dragon", Frequency::Default).next_delay(&mut rng), Duration::ZERO);
    }

    #[test]
    fn custom_range_is_used_and_sanitized() {
        let s = Settings {
            frequency: Frequency::Custom,
            custom_min_minutes: 2.0,
            custom_max_minutes: 1.0,
            volume: 7.0,
            situation: " ".into(),
            ..Settings::default()
        }
        .sanitized();
        assert_eq!((s.custom_min_minutes, s.custom_max_minutes, s.volume), (2.0, 2.0, 1.0));
        assert_eq!(s.situation, DEFAULT_SITUATION);
        assert_eq!(s.next_delay(&mut fastrand::Rng::new()), Duration::from_secs(120));
    }

    #[test]
    fn unknown_fields_and_missing_values_fall_back_to_defaults() {
        let s: Settings = serde_json::from_str(r#"{"volume": 0.5, "somethingNew": true}"#).unwrap();
        assert_eq!(s.volume, 0.5);
        assert_eq!(s.situation, DEFAULT_SITUATION);
        assert!(s.close_to_tray);
    }

    #[test]
    fn music_folders_use_overworld_timing() {
        let s = Settings { source: SourceMode::Folder, ..with("music.menu", Frequency::Default) };
        assert_eq!(s.max_delay(), Duration::from_secs(1200));
        assert!(!s.timing().replace_current);
    }

    #[test]
    fn replacing_music_stops_the_current_song() {
        assert!(timing("music.menu").replace_current);
        assert!(timing("music.end").replace_current);
        assert!(!timing("music.game").replace_current);
        assert!(!timing("music.overworld.cherry_grove").replace_current);
    }
}
