// Writes the music catalog of the latest Minecraft release as JSON, for the
// web version. Mojang's metadata servers refuse requests from web pages, so
// the catalog is built here instead, in CI. It holds names, hashes and
// weights only, never audio. Each track also gets the update that first had
// it, from data/song-history.json.
//
// Usage: node scripts/build-catalog.mjs <output.json>

import { readFile, writeFile } from "node:fs/promises";

import { MANIFEST_URL, get, songKey, versionMusic } from "./mojang.mjs";

const output = process.argv[2];
if (!output) {
  console.error("Usage: node scripts/build-catalog.mjs <output.json>");
  process.exit(2);
}

const history = JSON.parse(await readFile(new URL("../data/song-history.json", import.meta.url), "utf-8"));
const manifest = await get(MANIFEST_URL);
const id = manifest.latest.release;
const { catalog } = await versionMusic(id, manifest.versions.find((v) => v.id === id).url);

// Songs newer than the history file belong to this release.
catalog.updates = [...history.updates];
for (const track of Object.values(catalog.tracks)) {
  track.added = history.songs[songKey(track.title)]?.added ?? id;
}
if (Object.values(catalog.tracks).some((t) => t.added === id) && !catalog.updates.some((u) => u.id === id)) {
  catalog.updates.push({ id, name: null });
}

await writeFile(output, JSON.stringify(catalog));
console.log(`Minecraft ${id}: ${Object.keys(catalog.events).length} situations, ${Object.keys(catalog.tracks).length} tracks`);
