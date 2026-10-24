//! Minecraft's music metadata.
//!
//! The game describes its music in `sounds.json`: every *sound event* (for
//! example `music.game`, the overworld music) is a weighted list of sound files
//! or references to other events. The files themselves live in Mojang's
//! content-addressed asset store and are found through the version's asset
//! index.
//!
//! [`Catalog`] is a compact, flattened copy of only the music parts of that
//! data. It holds names, hashes and weights, never audio, so it is cheap to
//! cache on disk.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// Bump when the cached catalog layout changes.
pub const CATALOG_FORMAT: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Catalog {
    pub format: u32,
    /// Game version the data was read from, e.g. "26.3".
    pub version: String,
    /// Music events with event references already resolved.
    pub events: BTreeMap<String, Vec<Entry>>,
    /// Every sound file referenced by `events`, keyed by sound path.
    pub tracks: BTreeMap<String, TrackInfo>,
}

/// One weighted choice inside a music event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Sound path without namespace or extension, e.g. "music/game/sweden".
    pub path: String,
    pub weight: u32,
    /// Per-track gain the game applies on top of the music slider.
    pub volume: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrackInfo {
    /// "Artist - Title", as shown by the game's music toast.
    pub title: String,
    /// SHA-1 of the .ogg file in the asset store.
    pub hash: String,
    pub size: u64,
}

/// An `objects` entry of an asset index.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
pub struct AssetIndex {
    pub objects: HashMap<String, AssetObject>,
}

#[derive(Debug, Deserialize)]
struct SoundEventDef {
    #[serde(default)]
    sounds: Vec<SoundDef>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SoundDef {
    Name(String),
    Full {
        name: String,
        #[serde(default = "one")]
        volume: f32,
        #[serde(default = "one_u32")]
        weight: u32,
        #[serde(default, rename = "type")]
        kind: Option<String>,
    },
}

fn one() -> f32 {
    1.0
}

fn one_u32() -> u32 {
    1
}

/// Asset index key of a sound path, e.g. "minecraft/sounds/music/game/sweden.ogg".
pub fn asset_key(path: &str) -> String {
    format!("minecraft/sounds/{path}.ogg")
}

/// Splits "minecraft:music/game/x" into ("minecraft", "music/game/x").
fn split_namespace(name: &str) -> (&str, &str) {
    match name.split_once(':') {
        Some((ns, path)) => (ns, path),
        None => ("minecraft", name),
    }
}

impl Catalog {
    /// Builds a catalog from the raw `sounds.json`, the asset index objects and
    /// an optional language file used for track titles.
    pub fn build(
        version: &str,
        sounds_json: &[u8],
        objects: &HashMap<String, AssetObject>,
        lang: &HashMap<String, String>,
    ) -> Result<Catalog, String> {
        let defs: HashMap<String, SoundEventDef> =
            serde_json::from_slice(sounds_json).map_err(|e| format!("sounds.json is not valid: {e}"))?;

        let mut catalog = Catalog { format: CATALOG_FORMAT, version: version.to_string(), ..Default::default() };
        for event in defs.keys().filter(|k| k.starts_with("music.")) {
            let mut flat = Vec::new();
            flatten(&defs, event, 1.0, 0, &mut flat);
            let entries: Vec<Entry> = flat
                .into_iter()
                .filter_map(|(path, weight, volume)| {
                    let object = objects.get(&asset_key(&path))?;
                    catalog.tracks.entry(path.clone()).or_insert_with(|| TrackInfo {
                        title: title_for(&path, lang),
                        hash: object.hash.clone(),
                        size: object.size,
                    });
                    Some(Entry { path, weight, volume })
                })
                .collect();
            if entries.iter().any(|e| e.weight > 0) {
                catalog.events.insert(event.clone(), entries);
            }
        }
        if catalog.events.is_empty() {
            return Err(format!("Minecraft {version} has no playable music"));
        }
        Ok(catalog)
    }

    /// Weighted random choice, the way the game picks a sound from an event.
    /// `avoid` skips one track (used when skipping) unless it is the only one.
    pub fn pick(&self, event: &str, rng: &mut fastrand::Rng, avoid: Option<&str>) -> Option<&Entry> {
        let entries = self.events.get(event)?;
        let mut pool: Vec<&Entry> = entries.iter().filter(|e| Some(e.path.as_str()) != avoid).collect();
        if pool.iter().all(|e| e.weight == 0) {
            pool = entries.iter().collect();
        }
        let total: u64 = pool.iter().map(|e| u64::from(e.weight)).sum();
        if total == 0 {
            return None;
        }
        let mut roll = rng.u64(0..total);
        for entry in pool {
            let weight = u64::from(entry.weight);
            if roll < weight {
                return Some(entry);
            }
            roll -= weight;
        }
        None
    }

    /// Number of distinct tracks a situation can play.
    pub fn track_count(&self, event: &str) -> usize {
        let mut paths: Vec<&str> =
            self.events.get(event).map(|entries| entries.iter().map(|e| e.path.as_str()).collect()).unwrap_or_default();
        paths.sort_unstable();
        paths.dedup();
        paths.len()
    }
}

/// Resolves an event into (path, weight, volume) triples. A referenced event
/// contributes all of its entries, which gives the same odds as the game's
/// nested weighted lists.
fn flatten(
    defs: &HashMap<String, SoundEventDef>,
    event: &str,
    volume: f32,
    depth: u8,
    out: &mut Vec<(String, u32, f32)>,
) {
    let Some(def) = defs.get(event) else { return };
    if depth > 8 {
        return;
    }
    for sound in &def.sounds {
        match sound {
            SoundDef::Name(name) => {
                let (_, path) = split_namespace(name);
                out.push((path.to_string(), 1, volume));
            }
            SoundDef::Full { name, volume: v, weight, kind } => {
                let (_, path) = split_namespace(name);
                if kind.as_deref() == Some("event") {
                    flatten(defs, path, volume * v, depth + 1, out);
                } else {
                    out.push((path.to_string(), *weight, volume * v));
                }
            }
        }
    }
}

/// Extracts `music.*` titles from a game language file.
pub fn parse_lang(json: &[u8]) -> HashMap<String, String> {
    serde_json::from_slice::<HashMap<String, String>>(json)
        .map(|all| all.into_iter().filter(|(k, _)| k.starts_with("music.")).collect())
        .unwrap_or_default()
}

/// The game's language files name each track under its path with dots,
/// e.g. `music.game.sweden` = "C418 - Sweden".
fn title_for(path: &str, lang: &HashMap<String, String>) -> String {
    let key = path.replace('/', ".");
    if let Some(title) = lang.get(&key) {
        return title.clone();
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    if let Some((_, title)) = KNOWN_TITLES.iter().find(|(name, _)| *name == key || *name == file) {
        return (*title).to_string();
    }
    file.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fallback titles. Versions before 1.21.6 used numbered file names and had
/// no titles in their language files.
const KNOWN_TITLES: &[(&str, &str)] = &[
    ("calm1", "C418 - Minecraft"),
    ("calm2", "C418 - Clark"),
    ("calm3", "C418 - Sweden"),
    ("hal1", "C418 - Subwoofer Lullaby"),
    ("hal2", "C418 - Living Mice"),
    ("hal3", "C418 - Haggstrom"),
    ("hal4", "C418 - Danny"),
    ("nuance1", "C418 - Key"),
    ("nuance2", "C418 - Oxygène"),
    ("piano1", "C418 - Dry Hands"),
    ("piano2", "C418 - Wet Hands"),
    ("piano3", "C418 - Mice on Venus"),
    ("menu1", "C418 - Mutation"),
    ("menu2", "C418 - Moog City 2"),
    ("menu3", "C418 - Beginning 2"),
    ("menu4", "C418 - Floating Trees"),
    ("creative1", "C418 - Biome Fest"),
    ("creative2", "C418 - Blind Spots"),
    ("creative3", "C418 - Haunt Muskie"),
    ("creative4", "C418 - Aria Math"),
    ("creative5", "C418 - Dreiton"),
    ("creative6", "C418 - Taswell"),
    ("nether1", "C418 - Concrete Halls"),
    ("nether2", "C418 - Dead Voxel"),
    ("nether3", "C418 - Warmth"),
    ("nether4", "C418 - Ballad of the Cats"),
    ("music.game.end.end", "C418 - The End"),
    ("music.game.end.boss", "C418 - Boss"),
    ("music.game.end.credits", "C418 - Alpha"),
    ("axolotl", "C418 - Axolotl"),
    ("dragon_fish", "C418 - Dragon Fish"),
    ("shuniji", "C418 - Shuniji"),
    ("oxygene", "C418 - Oxygène"),
    ("os_piano", "Amos Roddy - O's Piano"),
];

/// Splits "C418 - Sweden" into ("Sweden", Some("C418")).
pub fn split_title(full: &str) -> (String, Option<String>) {
    match full.split_once(" - ") {
        Some((artist, title)) => (title.to_string(), Some(artist.to_string())),
        None => (full.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOUNDS: &str = r#"{
        "music.game": { "sounds": [
            { "name": "music/game/sweden", "stream": true },
            { "name": "minecraft:music/game/featherfall", "stream": true, "volume": 0.4 },
            { "name": "music/game/home", "stream": true, "volume": 0.4, "weight": 2 },
            { "name": "music/game/missing", "stream": true }
        ]},
        "music.creative": { "sounds": [
            { "name": "music.game", "type": "event" },
            { "name": "music/game/creative/taswell", "stream": true }
        ]},
        "music.menu": { "sounds": [ "music/menu/mutation" ] },
        "music.nether.warped_forest": { "sounds": [] },
        "music_disc.cat": { "sounds": [ { "name": "records/cat", "stream": true } ] },
        "block.stone.break": { "sounds": [ "dig/stone1" ] }
    }"#;

    fn objects() -> HashMap<String, AssetObject> {
        [
            "music/game/sweden",
            "music/game/featherfall",
            "music/game/home",
            "music/game/creative/taswell",
            "music/menu/mutation",
            "records/cat",
        ]
        .iter()
        .enumerate()
        .map(|(i, p)| (asset_key(p), AssetObject { hash: format!("{i:040}"), size: 100 + i as u64 }))
        .collect()
    }

    fn catalog() -> Catalog {
        let lang = parse_lang(br#"{"music.game.sweden": "C418 - Sweden", "block.x": "Stone"}"#);
        Catalog::build("26.3", SOUNDS.as_bytes(), &objects(), &lang).unwrap()
    }

    #[test]
    fn keeps_only_playable_music_events() {
        let c = catalog();
        let events: Vec<&str> = c.events.keys().map(String::as_str).collect();
        assert_eq!(events, ["music.creative", "music.game", "music.menu"]);
        assert_eq!(c.track_count("music.game"), 3, "missing assets are skipped");
        assert!(!c.tracks.contains_key("records/cat"));
    }

    #[test]
    fn resolves_event_references_with_weights_and_volume() {
        let c = catalog();
        let creative = &c.events["music.creative"];
        assert_eq!(creative.len(), 4);
        let home = creative.iter().find(|e| e.path == "music/game/home").unwrap();
        assert_eq!((home.weight, home.volume), (2, 0.4));
        let menu = &c.events["music.menu"][0];
        assert_eq!((menu.path.as_str(), menu.weight, menu.volume), ("music/menu/mutation", 1, 1.0));
    }

    #[test]
    fn titles_come_from_lang_then_builtin_then_file_name() {
        let c = catalog();
        assert_eq!(c.tracks["music/game/sweden"].title, "C418 - Sweden");
        assert_eq!(c.tracks["music/menu/mutation"].title, "Mutation");
        assert_eq!(title_for("music/game/calm3", &HashMap::new()), "C418 - Sweden");
        assert_eq!(title_for("music/game/end/boss", &HashMap::new()), "C418 - Boss");
        assert_eq!(title_for("music/game/below_and_above", &HashMap::new()), "Below And Above");
        assert_eq!(split_title("C418 - Sweden"), ("Sweden".into(), Some("C418".into())));
    }

    #[test]
    fn weighted_pick_matches_weights() {
        let c = catalog();
        let mut rng = fastrand::Rng::with_seed(7);
        let mut counts: HashMap<String, u32> = HashMap::new();
        for _ in 0..40_000 {
            let e = c.pick("music.game", &mut rng, None).unwrap();
            *counts.entry(e.path.clone()).or_default() += 1;
        }
        // Weights are 1, 1 and 2 out of 4.
        let home = f64::from(counts["music/game/home"]) / 40_000.0;
        let sweden = f64::from(counts["music/game/sweden"]) / 40_000.0;
        assert!((home - 0.5).abs() < 0.02, "home: {home}");
        assert!((sweden - 0.25).abs() < 0.02, "sweden: {sweden}");
    }

    #[test]
    fn pick_can_avoid_the_current_track() {
        let c = catalog();
        let mut rng = fastrand::Rng::with_seed(1);
        for _ in 0..200 {
            assert_ne!(c.pick("music.game", &mut rng, Some("music/game/home")).unwrap().path, "music/game/home");
        }
        // A single-track event still plays its only track.
        assert_eq!(c.pick("music.menu", &mut rng, Some("music/menu/mutation")).unwrap().path, "music/menu/mutation");
        assert!(c.pick("music.unknown", &mut rng, None).is_none());
    }

    #[test]
    fn catalog_roundtrips_through_json() {
        let c = catalog();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Catalog>(&json).unwrap(), c);
    }
}
