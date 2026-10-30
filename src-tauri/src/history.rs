//! Which update first had each song, and which songs play by default.
//!
//! The history comes from `data/song-history.json`, built by
//! `scripts/song-history.mjs` from every release's asset index and built into
//! the app. Songs are matched by name, since their files and titles changed
//! over the years ("calm3" became "sweden", "Oxygène" lost its accent).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::catalog::{split_title, Catalog};

const HISTORY_JSON: &str = include_str!("../../data/song-history.json");

#[derive(Deserialize)]
struct History {
    updates: Vec<Update>,
    songs: HashMap<String, Added>,
}

#[derive(Deserialize)]
struct Added {
    added: String,
}

/// A game update that brought new music.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Update {
    /// The first release with the music, e.g. "1.18".
    pub id: String,
    /// E.g. "Caves & Cliffs: Part II".
    pub name: Option<String>,
}

/// One song as the settings list shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Song {
    pub key: String,
    pub title: String,
    pub artist: Option<String>,
    /// Id of the update that added it.
    pub update: String,
    /// Checked when the user has not chosen songs.
    pub default: bool,
    /// Whether the current music can play it.
    pub here: bool,
}

/// Identifies a song across versions: its name without the artist, accents,
/// case or punctuation. The same rule is in scripts/mojang.mjs and src/web.js.
pub fn song_key(title: &str) -> String {
    let name = title.split_once(" - ").map_or(title, |(_, name)| name);
    name.to_lowercase()
        .chars()
        .map(|c| match c {
            'à'..='å' => 'a',
            'ç' => 'c',
            'è'..='ë' => 'e',
            'ì'..='ï' => 'i',
            'ñ' => 'n',
            'ò'..='ö' | 'ø' => 'o',
            'ù'..='ü' => 'u',
            'ý' | 'ÿ' => 'y',
            c => c,
        })
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

struct Entry {
    key: String,
    title: String,
    artist: Option<String>,
    update: String,
    rank: usize,
}

/// The songs of one catalog.
pub struct Songs {
    /// Song key of each sound path.
    by_path: HashMap<String, String>,
    /// In update order, then by title.
    list: Vec<Entry>,
    defaults: HashSet<String>,
    pub updates: Vec<Update>,
}

impl Songs {
    pub fn new(catalog: &Catalog) -> Songs {
        let history: History = serde_json::from_str(HISTORY_JSON).expect("data/song-history.json is valid");
        Songs::with_history(catalog, history)
    }

    fn with_history(catalog: &Catalog, history: History) -> Songs {
        let mut updates = history.updates;
        let mut by_path = HashMap::new();
        let mut list: Vec<Entry> = Vec::new();
        for (path, track) in &catalog.tracks {
            let key = song_key(&track.title);
            by_path.insert(path.clone(), key.clone());
            if list.iter().any(|e| e.key == key) {
                continue;
            }
            // Songs newer than the history belong to this catalog's version.
            let update = history.songs.get(&key).map_or_else(|| catalog.version.clone(), |s| s.added.clone());
            if !updates.iter().any(|u| u.id == update) {
                updates.push(Update { id: update.clone(), name: None });
            }
            let (title, artist) = split_title(&track.title);
            list.push(Entry { key, title, artist, update, rank: 0 });
        }
        for entry in &mut list {
            entry.rank = updates.iter().position(|u| u.id == entry.update).unwrap_or(updates.len());
        }
        list.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.title.cmp(&b.title)));
        updates.retain(|u| list.iter().any(|e| e.update == u.id));

        // Each kind of music plays its songs from the earliest update that had it.
        let rank: HashMap<&str, usize> = list.iter().map(|e| (e.key.as_str(), e.rank)).collect();
        let mut defaults = HashSet::new();
        for entries in catalog.events.values() {
            let keys: Vec<&str> = entries.iter().filter_map(|e| by_path.get(&e.path)).map(String::as_str).collect();
            let first = keys.iter().map(|k| rank[k]).min();
            defaults.extend(keys.iter().filter(|k| Some(rank[*k]) == first).map(|k| k.to_string()));
        }
        Songs { by_path, list, defaults, updates }
    }

    pub fn key(&self, path: &str) -> Option<&str> {
        self.by_path.get(path).map(String::as_str)
    }

    /// The songs that may play: the user's choice, or the defaults.
    pub fn allowed(&self, chosen: Option<&[String]>) -> HashSet<String> {
        match chosen {
            Some(keys) => keys.iter().cloned().collect(),
            None => self.defaults.clone(),
        }
    }

    pub fn list(&self, catalog: &Catalog, situation: &str) -> Vec<Song> {
        let here: HashSet<&str> = catalog
            .events
            .get(situation)
            .map(|entries| entries.iter().filter_map(|e| self.key(&e.path)).collect())
            .unwrap_or_default();
        self.list
            .iter()
            .map(|e| Song {
                key: e.key.clone(),
                title: e.title.clone(),
                artist: e.artist.clone(),
                update: e.update.clone(),
                default: self.defaults.contains(&e.key),
                here: here.contains(e.key.as_str()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::catalog::{Entry as CatalogEntry, TrackInfo};

    fn catalog() -> Catalog {
        let track = |title: &str| TrackInfo { title: title.into(), hash: "0".repeat(40), size: 1 };
        let entry = |path: &str| CatalogEntry { path: path.into(), weight: 1, volume: 1.0 };
        let tracks: BTreeMap<String, TrackInfo> = [
            ("music/game/sweden", "C418 - Sweden"),
            ("music/game/oxygene", "C418 - Oxygène"),
            ("music/game/wending", "Lena Raine - Wending"),
            ("music/game/brand_new", "Someone - Brand New"),
            ("music/game/nether/rubedo", "Lena Raine - Rubedo"),
        ]
        .into_iter()
        .map(|(path, title)| (path.to_string(), track(title)))
        .collect();
        let mut events = BTreeMap::new();
        events.insert(
            "music.game".to_string(),
            ["music/game/sweden", "music/game/oxygene", "music/game/wending", "music/game/brand_new"]
                .map(entry)
                .to_vec(),
        );
        events.insert("music.nether".to_string(), vec![entry("music/game/nether/rubedo")]);
        Catalog { format: 1, version: "26.9".into(), events, tracks }
    }

    fn history() -> History {
        serde_json::from_str(
            r#"{"updates": [{"id": "1.0", "name": "Original"}, {"id": "1.16", "name": null},
                            {"id": "1.18", "name": "Caves & Cliffs: Part II"}, {"id": "1.19", "name": null}],
                "songs": {"sweden": {"added": "1.0"}, "oxygene": {"added": "1.0"},
                          "rubedo": {"added": "1.16"}, "wending": {"added": "1.18"}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn keys_ignore_artist_accents_and_case() {
        assert_eq!(song_key("C418 - Oxygène"), "oxygene");
        assert_eq!(song_key("Oxygene"), "oxygene");
        assert_eq!(song_key("Amos Roddy - O's Piano"), "ospiano");
        assert_eq!(song_key("Kumi Tanioka - komorebi"), "komorebi");
    }

    #[test]
    fn songs_are_grouped_by_update_with_the_earliest_as_default() {
        let c = catalog();
        let songs = Songs::with_history(&c, history());
        let list = songs.list(&c, "music.game");
        let names: Vec<(&str, &str, bool, bool)> =
            list.iter().map(|s| (s.title.as_str(), s.update.as_str(), s.default, s.here)).collect();
        assert_eq!(
            names,
            [
                ("Oxygène", "1.0", true, true),
                ("Sweden", "1.0", true, true),
                ("Rubedo", "1.16", true, false),
                ("Wending", "1.18", false, true),
                ("Brand New", "26.9", false, true),
            ]
        );
        let ids: Vec<&str> = songs.updates.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, ["1.0", "1.16", "1.18", "26.9"], "updates without songs here are left out");
    }

    #[test]
    fn chosen_songs_replace_the_defaults() {
        let songs = Songs::with_history(&catalog(), history());
        assert!(songs.allowed(None).contains("sweden"));
        assert!(!songs.allowed(None).contains("wending"));
        let chosen = ["wending".to_string()];
        assert_eq!(songs.allowed(Some(&chosen)), HashSet::from(["wending".to_string()]));
        assert_eq!(songs.key("music/game/oxygene"), Some("oxygene"));
    }

    #[test]
    fn built_in_history_is_valid() {
        let history: History = serde_json::from_str(HISTORY_JSON).unwrap();
        assert_eq!(history.updates[0].id, "1.0");
        assert_eq!(history.songs["sweden"].added, "1.0");
    }
}
