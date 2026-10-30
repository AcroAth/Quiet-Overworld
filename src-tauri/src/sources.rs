//! Where music comes from: Mojang's asset servers, a local game install, or
//! a folder of the user's own files.
//!
//! Nothing here writes audio to disk. Streamed tracks are kept in memory only
//! while they play.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::catalog::{parse_lang, AssetIndex, AssetObject, Catalog};

pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const RESOURCES_URL: &str = "https://resources.download.minecraft.net";

const SOUNDS_KEY: &str = "minecraft/sounds.json";
/// en_us.json ships inside the game jar; en_gb.json is in the asset store
/// and has the same track titles.
const LANG_KEY: &str = "minecraft/lang/en_gb.json";
const MB: u64 = 1024 * 1024;

#[derive(Deserialize)]
struct Manifest {
    latest: Latest,
    versions: Vec<VersionRef>,
}

#[derive(Deserialize)]
struct Latest {
    release: String,
}

#[derive(Deserialize)]
struct VersionRef {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

#[derive(Deserialize)]
struct VersionJson {
    #[serde(rename = "assetIndex")]
    asset_index: Option<IndexRef>,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(rename = "releaseTime", default)]
    release_time: String,
}

#[derive(Deserialize)]
struct IndexRef {
    id: String,
    #[serde(default)]
    url: String,
}

/// Client for Mojang's public launcher metadata and asset servers.
pub struct Mojang {
    agent: ureq::Agent,
    manifest_url: String,
    resources_url: String,
}

impl Default for Mojang {
    fn default() -> Self {
        Self::with_urls(MANIFEST_URL, RESOURCES_URL)
    }
}

impl Mojang {
    pub fn with_urls(manifest_url: &str, resources_url: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(90)))
            .user_agent(concat!("QuietOverworld/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Mojang {
            agent,
            manifest_url: manifest_url.trim_end_matches('/').to_string(),
            resources_url: resources_url.trim_end_matches('/').to_string(),
        }
    }

    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        let mut response = self.agent.get(url).call().map_err(|e| describe_http_error(url, e))?;
        response.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| describe_http_error(url, e))
    }

    fn manifest(&self) -> Result<Manifest, String> {
        let bytes = self.get(&self.manifest_url, 32 * MB)?;
        serde_json::from_slice(&bytes).map_err(|e| format!("Unexpected version list from Mojang: {e}"))
    }

    /// Release version ids, newest first.
    pub fn releases(&self) -> Result<Vec<String>, String> {
        let manifest = self.manifest()?;
        Ok(manifest.versions.into_iter().filter(|v| v.kind == "release").map(|v| v.id).collect())
    }

    /// Resolves a version id ("" means the latest release) to the id and the
    /// URL of its version JSON.
    pub fn resolve(&self, version: &str) -> Result<(String, String), String> {
        let manifest = self.manifest()?;
        let wanted = if version.is_empty() { manifest.latest.release.as_str() } else { version };
        manifest
            .versions
            .iter()
            .find(|v| v.id == wanted)
            .map(|v| (v.id.clone(), v.url.clone()))
            .ok_or_else(|| format!("Minecraft {wanted} was not found in Mojang's version list"))
    }

    /// Downloads the music catalog of one version: its asset index, sounds.json
    /// and a language file for track titles (a few hundred kB of JSON).
    pub fn catalog(&self, version_id: &str, version_url: &str) -> Result<Catalog, String> {
        let version: VersionJson = serde_json::from_slice(&self.get(version_url, 16 * MB)?)
            .map_err(|e| format!("Unexpected data for Minecraft {version_id}: {e}"))?;
        let index_ref = version.asset_index.ok_or_else(|| format!("Minecraft {version_id} has no asset index"))?;
        let index: AssetIndex = serde_json::from_slice(&self.get(&index_ref.url, 64 * MB)?)
            .map_err(|e| format!("Unexpected asset index for Minecraft {version_id}: {e}"))?;
        let sounds =
            index.objects.get(SOUNDS_KEY).ok_or_else(|| format!("Minecraft {version_id} has no sounds.json"))?;
        let sounds = self.fetch_object(sounds)?;
        let lang = index
            .objects
            .get(LANG_KEY)
            .and_then(|object| self.fetch_object(object).ok())
            .map(|bytes| parse_lang(&bytes))
            .unwrap_or_default();
        Catalog::build(version_id, &sounds, &index.objects, &lang)
    }

    /// Downloads one object into memory and checks its SHA-1.
    pub fn fetch_object(&self, object: &AssetObject) -> Result<Vec<u8>, String> {
        let hash = valid_hash(&object.hash)?;
        let url = format!("{}/{}/{}", self.resources_url, &hash[..2], hash);
        let bytes = self.get(&url, object.size.max(1) + MB)?;
        if sha1_smol::Sha1::from(&bytes).digest().to_string() != hash {
            return Err("Download was corrupted, will try again later".into());
        }
        Ok(bytes)
    }
}

fn valid_hash(hash: &str) -> Result<&str, String> {
    if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(hash)
    } else {
        Err(format!("Invalid asset hash {hash:?}"))
    }
}

fn describe_http_error(url: &str, error: ureq::Error) -> String {
    let host = url.split('/').nth(2).unwrap_or(url);
    match error {
        ureq::Error::StatusCode(code) => format!("{host} answered HTTP {code}"),
        ureq::Error::Io(e) => format!("Could not reach {host}: {e}"),
        ureq::Error::Timeout(_) => format!("{host} timed out"),
        other => format!("Could not reach {host}: {other}"),
    }
}

/// A Minecraft Java Edition data folder (`.minecraft`, or a launcher's data
/// folder) whose `assets` directory holds the game's music.
#[derive(Clone, Debug)]
pub struct LocalInstall {
    root: PathBuf,
}

impl LocalInstall {
    /// Where launchers keep game assets by default on this platform.
    pub fn default_roots(home: Option<&Path>, data_dir: Option<&Path>) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(data) = data_dir {
            // %APPDATA%\.minecraft on Windows, ~/Library/Application Support/minecraft on macOS.
            roots.push(data.join(".minecraft"));
            roots.push(data.join("minecraft"));
            roots.push(data.join("PrismLauncher"));
            roots.push(data.join("multimc"));
        }
        if let Some(home) = home {
            roots.push(home.join(".minecraft"));
            roots.push(home.join(".var/app/com.mojang.Minecraft/.minecraft"));
            roots.push(home.join(".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher"));
        }
        roots
    }

    /// Returns the first folder that contains downloaded game assets.
    pub fn find(candidates: &[PathBuf]) -> Option<LocalInstall> {
        candidates.iter().find_map(|root| LocalInstall::open(root))
    }

    pub fn open(root: &Path) -> Option<LocalInstall> {
        let assets = root.join("assets");
        (assets.join("objects").is_dir() && assets.join("indexes").is_dir())
            .then(|| LocalInstall { root: root.to_path_buf() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of an object if the game has already downloaded it.
    pub fn object(&self, object: &AssetObject) -> Option<PathBuf> {
        let hash = valid_hash(&object.hash).ok()?;
        let path = self.root.join("assets/objects").join(&hash[..2]).join(hash);
        let size = fs::metadata(&path).ok()?.len();
        (size == object.size).then_some(path)
    }

    fn read_object(&self, object: &AssetObject) -> Result<Vec<u8>, String> {
        let path = self.object(object).ok_or("A game file is missing, try launching the game once")?;
        fs::read(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))
    }

    /// Builds the catalog from installed files only, without network access.
    /// An empty `version` picks the newest installed release.
    pub fn catalog(&self, version: &str) -> Result<Catalog, String> {
        let (label, index_id) = self.pick_index(version)?;
        let index_path = self.root.join("assets/indexes").join(format!("{index_id}.json"));
        let index: AssetIndex = fs::read(&index_path)
            .map_err(|e| format!("Could not read {}: {e}", index_path.display()))
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| format!("Invalid asset index: {e}")))?;
        let sounds =
            index.objects.get(SOUNDS_KEY).ok_or_else(|| format!("The installed game ({label}) has no sounds.json"))?;
        let sounds = self.read_object(sounds)?;
        let lang: HashMap<String, String> = index
            .objects
            .get(LANG_KEY)
            .and_then(|object| self.read_object(object).ok())
            .map(|bytes| parse_lang(&bytes))
            .unwrap_or_default();
        let mut catalog = Catalog::build(&label, &sounds, &index.objects, &lang)?;
        // Only keep tracks that are actually on disk.
        let tracks = &catalog.tracks;
        let installed: Vec<String> = tracks
            .iter()
            .filter(|(_, t)| self.object(&AssetObject { hash: t.hash.clone(), size: t.size }).is_some())
            .map(|(path, _)| path.clone())
            .collect();
        catalog.tracks.retain(|path, _| installed.contains(path));
        for entries in catalog.events.values_mut() {
            entries.retain(|e| installed.contains(&e.path));
        }
        catalog.events.retain(|_, entries| !entries.is_empty());
        if catalog.events.is_empty() {
            return Err(format!("The installed game ({label}) has no music downloaded"));
        }
        Ok(catalog)
    }

    /// Chooses an asset index: the requested version's, or the newest
    /// installed release's, or the most recently written index file.
    fn pick_index(&self, version: &str) -> Result<(String, String), String> {
        let versions_dir = self.root.join("versions");
        let index_exists = |id: &str| self.root.join("assets/indexes").join(format!("{id}.json")).is_file();
        let read_version = |id: &str| -> Option<VersionJson> {
            let bytes = fs::read(versions_dir.join(id).join(format!("{id}.json"))).ok()?;
            serde_json::from_slice(&bytes).ok()
        };

        if !version.is_empty() {
            return read_version(version)
                .and_then(|v| v.asset_index)
                .filter(|index| index_exists(&index.id))
                .map(|index| (version.to_string(), index.id))
                .ok_or_else(|| format!("Minecraft {version} is not installed in {}", self.root.display()));
        }

        let mut best: Option<(bool, String, String, String)> = None;
        for entry in fs::read_dir(&versions_dir).into_iter().flatten().flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let Some(json) = read_version(&id) else { continue };
            let Some(index) = json.asset_index.filter(|i| index_exists(&i.id)) else { continue };
            let key = (json.kind == "release", json.release_time, id, index.id);
            if best.as_ref().is_none_or(|b| (b.0, &b.1) < (key.0, &key.1)) {
                best = Some(key);
            }
        }
        if let Some((_, _, id, index)) = best {
            return Ok((id, index));
        }

        let newest = fs::read_dir(self.root.join("assets/indexes"))
            .map_err(|e| format!("Could not read the game's assets: {e}"))?
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
        let id = newest
            .map(|e| e.path().file_stem().unwrap_or_default().to_string_lossy().into_owned())
            .ok_or("The game has not downloaded any assets yet")?;
        Ok((format!("asset index {id}"), id))
    }
}

const AUDIO_EXTENSIONS: &[&str] = &["ogg", "oga", "mp3", "flac", "wav"];

/// Lists audio files in a folder and its subfolders.
pub fn scan_folder(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("Folder not found: {}", dir.display()));
    }
    let mut files = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), 0)];
    while let Some((folder, depth)) = pending.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 4 {
                    pending.push((path, depth + 1));
                }
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
            {
                files.push(path);
            }
        }
    }
    if files.is_empty() {
        return Err(format!("No audio files (.ogg, .mp3, .flac, .wav) in {}", dir.display()));
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("quiet-overworld-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put_object(root: &Path, bytes: &[u8]) -> AssetObject {
        let hash = sha1_smol::Sha1::from(bytes).digest().to_string();
        let dir = root.join("assets/objects").join(&hash[..2]);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(&hash), bytes).unwrap();
        AssetObject { hash, size: bytes.len() as u64 }
    }

    #[test]
    fn local_install_builds_catalog_from_installed_files() {
        let root = temp_dir("local");
        let sounds = br#"{"music.game": {"sounds": ["music/game/sweden", "music/game/not_downloaded"]},
                          "music.menu": {"sounds": ["music/menu/not_downloaded"]}}"#;
        let sounds_obj = put_object(&root, sounds);
        let track = put_object(&root, b"fake ogg data");
        let lang = put_object(&root, br#"{"music.game.sweden": "C418 - Sweden"}"#);
        let index = serde_json::json!({ "objects": {
            "minecraft/sounds.json": sounds_obj,
            "minecraft/lang/en_gb.json": lang,
            "minecraft/sounds/music/game/sweden.ogg": track,
            "minecraft/sounds/music/game/not_downloaded.ogg": {"hash": "a".repeat(40), "size": 5},
            "minecraft/sounds/music/menu/not_downloaded.ogg": {"hash": "b".repeat(40), "size": 5},
        }});
        fs::create_dir_all(root.join("assets/indexes")).unwrap();
        fs::write(root.join("assets/indexes/29.json"), index.to_string()).unwrap();
        for (id, kind, time, idx) in [
            ("26.3", "release", "2026-09-15", "29"),
            ("26.4-snapshot-5", "snapshot", "2026-10-20", "29"),
            ("1.8.9", "release", "2015-12-09", "1.8"),
        ] {
            fs::create_dir_all(root.join("versions").join(id)).unwrap();
            let json = serde_json::json!({"id": id, "type": kind, "releaseTime": time, "assetIndex": {"id": idx}});
            fs::write(root.join("versions").join(id).join(format!("{id}.json")), json.to_string()).unwrap();
        }

        let install = LocalInstall::find(&[root.join("nope"), root.clone()]).expect("install found");
        let catalog = install.catalog("").unwrap();
        assert_eq!(catalog.version, "26.3", "newest release with an installed index");
        assert_eq!(catalog.events.keys().collect::<Vec<_>>(), ["music.game"]);
        assert_eq!(catalog.tracks["music/game/sweden"].title, "C418 - Sweden");
        assert!(install.object(&track).is_some());
        assert!(install.catalog("1.8.9").is_err(), "index 1.8 is not installed");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_folder_finds_audio_files_recursively() {
        let dir = temp_dir("folder");
        fs::create_dir_all(dir.join("Volume Alpha")).unwrap();
        fs::write(dir.join("Volume Alpha/03 Subwoofer Lullaby.MP3"), b"x").unwrap();
        fs::write(dir.join("cover.jpg"), b"x").unwrap();
        fs::write(dir.join("sweden.ogg"), b"x").unwrap();
        let files = scan_folder(&dir).unwrap();
        assert_eq!(files.len(), 2);
        assert!(scan_folder(&dir.join("missing")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_malformed_hashes() {
        assert!(valid_hash("../../etc/passwd").is_err());
        assert!(valid_hash(&"f".repeat(40)).is_ok());
    }
}
