// Finds the first release that had each Minecraft song and writes
// data/song-history.json. The app and the web version use it to group songs
// by the update that added them. Reads every release's asset index (metadata
// only, no audio); run by .github/workflows/song-history.yml.
//
// Usage: node scripts/song-history.mjs <output.json>

import { writeFile } from "node:fs/promises";

import { MANIFEST_URL, get, songKey, titleFor, versionMusic } from "./mojang.mjs";

// Names of the updates that brought new music.
const UPDATE_NAMES = {
  "1.0": "Original",
  "1.6.1": "Horse Update",
  "1.13": "Update Aquatic",
  "1.16": "Nether Update",
  "1.18": "Caves & Cliffs: Part II",
  "1.19": "The Wild Update",
  "1.20": "Trails & Tales",
  "1.21": "Tricky Trials",
  "1.21.4": "The Garden Awakens",
  "1.21.6": "Chase the Skies",
};

// Music files in an index without sounds.json: music/, newmusic/ and
// sounds/music/ folders, not records.
function legacyTitles(objects) {
  return Object.keys(objects)
    .filter((key) => /(^|\/)(new)?music\/.*\.ogg$/.test(key) && !/records|streaming/.test(key))
    .map((key) => titleFor(key.replace(/\.ogg$/, "").split("/").pop(), {}));
}

const output = process.argv[2];
if (!output) {
  console.error("Usage: node scripts/song-history.mjs <output.json>");
  process.exit(2);
}

const manifest = await get(MANIFEST_URL);
const releases = manifest.versions.filter((v) => v.type === "release").reverse();
const songs = {};
const updates = [];
const seenIndexes = new Map();

for (const release of releases) {
  const version = await get(release.url);
  const indexId = version.assetIndex?.id;
  if (!indexId) continue;
  // Versions sharing an asset index have the same music.
  let titles = seenIndexes.get(indexId);
  if (!titles) {
    const { catalog, objects } = await versionMusic(release.id, release.url).catch(() => ({}));
    titles = catalog ? Object.values(catalog.tracks).map((t) => t.title) : objects ? legacyTitles(objects) : [];
    seenIndexes.set(indexId, titles);
  }
  let added = false;
  for (const title of titles) {
    const key = songKey(title);
    if (!key) continue;
    if (!songs[key]) {
      songs[key] = { title, added: release.id };
      added = true;
    } else if (title.includes(" - ") && !songs[key].title.includes(" - ")) {
      songs[key].title = title; // Newer versions name the artist.
    }
  }
  if (added) {
    updates.push({ id: release.id, name: UPDATE_NAMES[release.id] ?? null });
    console.log(`${release.id}: ${Object.values(songs).filter((s) => s.added === release.id).map((s) => s.title).join(", ")}`);
  }
}

const sorted = Object.fromEntries(Object.entries(songs).sort(([a], [b]) => a.localeCompare(b)));
await writeFile(output, JSON.stringify({ latest: manifest.latest.release, updates, songs: sorted }, null, 1) + "\n");
console.log(`${Object.keys(songs).length} songs in ${updates.length} updates`);
