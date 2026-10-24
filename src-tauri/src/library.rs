//! The set of tracks the engine picks from, and how a picked track is fetched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::catalog::{split_title, AssetObject, Catalog, CATALOG_FORMAT};
use crate::settings::{Settings, SourceMode};
use crate::sources::{scan_folder, LocalInstall, Mojang};

/// How long a downloaded catalog of the latest release is trusted before
/// checking Mojang for a newer release.
pub const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

pub enum Library {
    Minecraft { catalog: Catalog, install: Option<LocalInstall>, mojang: Option<Arc<Mojang>> },
    Folder { files: Vec<PathBuf> },
}

/// Where the audio of a track came from, shown in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Streamed from Mojang's servers into memory.
    Stream,
    /// Read from the installed game.
    Installed,
    /// The user's own file.
    Folder,
}

/// Audio data, cheap to clone so a paused track can be reopened.
#[derive(Clone)]
pub enum Audio {
    Memory(Arc<[u8]>),
    File(PathBuf),
}

/// A track chosen to play next, not fetched yet.
pub struct Choice {
    /// Identifies the track, so skipping can avoid replaying it.
    pub key: String,
    pub title: String,
    pub artist: Option<String>,
    /// The game's per-track gain.
    pub volume: f32,
    fetch: Fetch,
}

enum Fetch {
    File(PathBuf, Origin),
    Stream(Arc<Mojang>, AssetObject),
    Unavailable,
}

impl Choice {
    /// Fetches the audio. Blocks on network or disk, so call it off the
    /// engine thread.
    pub fn load(&self) -> Result<(Audio, Origin), String> {
        match &self.fetch {
            Fetch::File(path, origin) => Ok((Audio::File(path.clone()), *origin)),
            Fetch::Stream(mojang, object) => Ok((Audio::Memory(mojang.fetch_object(object)?.into()), Origin::Stream)),
            Fetch::Unavailable => Err(format!("\"{}\" is not in the installed game", self.title)),
        }
    }

    /// File extension hint for the decoder.
    pub fn format_hint(&self) -> Option<String> {
        match &self.fetch {
            Fetch::File(path, Origin::Folder) => path.extension().map(|ext| ext.to_string_lossy().to_ascii_lowercase()),
            _ => Some("ogg".into()),
        }
    }
}

impl Library {
    pub fn pick(&self, situation: &str, rng: &mut fastrand::Rng, avoid: Option<&str>) -> Option<Choice> {
        match self {
            Library::Minecraft { catalog, install, mojang } => {
                let entry = catalog.pick(situation, rng, avoid)?;
                let track = catalog.tracks.get(&entry.path)?;
                let object = AssetObject { hash: track.hash.clone(), size: track.size };
                let fetch = match (install.as_ref().and_then(|i| i.object(&object)), mojang) {
                    (Some(path), _) => Fetch::File(path, Origin::Installed),
                    (None, Some(mojang)) => Fetch::Stream(mojang.clone(), object),
                    (None, None) => Fetch::Unavailable,
                };
                let (title, artist) = split_title(&track.title);
                Some(Choice { key: entry.path.clone(), title, artist, volume: entry.volume, fetch })
            }
            Library::Folder { files } => {
                let pool: Vec<&PathBuf> =
                    files.iter().filter(|f| Some(f.to_string_lossy().as_ref()) != avoid).collect();
                let file = if pool.is_empty() { files.first()? } else { pool[rng.usize(..pool.len())] };
                let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let (title, artist) = split_title(&name);
                Some(Choice {
                    key: file.to_string_lossy().into_owned(),
                    title,
                    artist,
                    volume: 1.0,
                    fetch: Fetch::File(file.clone(), Origin::Folder),
                })
            }
        }
    }

    /// Short description for the UI, e.g. "Minecraft 26.3 · 33 tracks".
    pub fn describe(&self, situation: &str) -> String {
        match self {
            Library::Minecraft { catalog, .. } => {
                let count = catalog.track_count(situation);
                let tracks = if count == 1 { "track" } else { "tracks" };
                format!("Minecraft {} · {count} {tracks}", catalog.version)
            }
            Library::Folder { files } => format!("Your music · {} files", files.len()),
        }
    }

    /// Music situations this library can play, in catalog order.
    pub fn situations(&self) -> Vec<String> {
        match self {
            Library::Minecraft { catalog, .. } => catalog.events.keys().cloned().collect(),
            Library::Folder { .. } => Vec::new(),
        }
    }

    /// Whether the library should be rebuilt later to pick up new releases.
    pub fn wants_refresh(&self, settings: &Settings) -> bool {
        matches!(self, Library::Minecraft { mojang: Some(_), .. }) && settings.game_version.is_empty()
    }
}

/// Everything needed to load a library, so it can be done on a worker thread.
pub struct LibraryRequest {
    pub source: SourceMode,
    pub game_version: String,
    pub install_roots: Vec<PathBuf>,
    pub music_folder: String,
    pub cache_file: PathBuf,
    pub mojang: Arc<Mojang>,
}

#[derive(Serialize, Deserialize)]
struct CachedCatalog {
    /// Unix time of the last successful check against Mojang.
    checked: u64,
    catalog: Catalog,
}

impl LibraryRequest {
    pub fn load(&self) -> Result<Library, String> {
        match self.source {
            SourceMode::Folder => {
                if self.music_folder.is_empty() {
                    return Err("Choose a music folder in the settings".into());
                }
                Ok(Library::Folder { files: scan_folder(Path::new(&self.music_folder))? })
            }
            SourceMode::Local => {
                let install = LocalInstall::find(&self.install_roots).ok_or_else(no_install_message)?;
                let catalog = install.catalog(&self.game_version)?;
                Ok(Library::Minecraft { catalog, install: Some(install), mojang: None })
            }
            SourceMode::Minecraft => {
                let install = LocalInstall::find(&self.install_roots);
                let catalog = match self.online_catalog() {
                    Ok(catalog) => catalog,
                    Err(online_error) => match &install {
                        Some(install) => install.catalog(&self.game_version).map_err(|_| online_error)?,
                        None => return Err(online_error),
                    },
                };
                Ok(Library::Minecraft { catalog, install, mojang: Some(self.mojang.clone()) })
            }
        }
    }

    /// The catalog from Mojang, using the on-disk copy when it is still good.
    fn online_catalog(&self) -> Result<Catalog, String> {
        let cached = fs::read(&self.cache_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CachedCatalog>(&bytes).ok())
            .filter(|c| c.catalog.format == CATALOG_FORMAT);
        if let Some(cached) = &cached {
            let fresh = now_secs().saturating_sub(cached.checked) < REFRESH_AFTER.as_secs();
            let wanted = if self.game_version.is_empty() { fresh } else { cached.catalog.version == self.game_version };
            if wanted {
                return Ok(cached.catalog.clone());
            }
        }

        // A stale copy of the latest release still beats no music when offline.
        let fallback = cached
            .filter(|c| self.game_version.is_empty() || c.catalog.version == self.game_version)
            .map(|c| c.catalog);
        let fetched = self.mojang.resolve(&self.game_version).and_then(|(id, url)| match &fallback {
            Some(catalog) if catalog.version == id => Ok(catalog.clone()),
            _ => self.mojang.catalog(&id, &url),
        });
        let catalog = match (fetched, fallback) {
            (Ok(catalog), _) => catalog,
            (Err(e), Some(catalog)) => {
                eprintln!("[quiet-overworld] using cached catalog: {e}");
                return Ok(catalog);
            }
            (Err(e), None) => return Err(e),
        };
        let entry = CachedCatalog { checked: now_secs(), catalog };
        if let Some(dir) = self.cache_file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(&entry) {
            let _ = fs::write(&self.cache_file, json);
        }
        Ok(entry.catalog)
    }
}

fn no_install_message() -> String {
    "No Minecraft Java Edition files found. Launch the game once, or set the game folder in the settings".into()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Picks where to look for an installed game.
pub fn install_roots(settings: &Settings, defaults: &[PathBuf]) -> Vec<PathBuf> {
    if settings.minecraft_dir.is_empty() {
        defaults.to_vec()
    } else {
        vec![PathBuf::from(&settings.minecraft_dir)]
    }
}
