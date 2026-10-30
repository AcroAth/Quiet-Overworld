// Writes the music catalog of the latest Minecraft release as JSON, for the
// web version. Mojang's metadata servers refuse requests from web pages, so
// the catalog is built here instead, in CI. It holds names, hashes and
// weights only, never audio. Mirrors src-tauri/src/catalog.rs and sources.rs.
//
// Usage: node scripts/build-catalog.mjs <output.json>

import { writeFile } from "node:fs/promises";

const MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const RESOURCES_URL = "https://resources.download.minecraft.net";
const SOUNDS_KEY = "minecraft/sounds.json";
const LANG_KEY = "minecraft/lang/en_gb.json";

// Fallback titles: versions before 1.21.6 used numbered file names and had no
// titles in their language files. Same list as catalog.rs.
const KNOWN_TITLES = {
  calm1: "C418 - Minecraft",
  calm2: "C418 - Clark",
  calm3: "C418 - Sweden",
  hal1: "C418 - Subwoofer Lullaby",
  hal2: "C418 - Living Mice",
  hal3: "C418 - Haggstrom",
  hal4: "C418 - Danny",
  nuance1: "C418 - Key",
  nuance2: "C418 - Oxygène",
  piano1: "C418 - Dry Hands",
  piano2: "C418 - Wet Hands",
  piano3: "C418 - Mice on Venus",
  menu1: "C418 - Mutation",
  menu2: "C418 - Moog City 2",
  menu3: "C418 - Beginning 2",
  menu4: "C418 - Floating Trees",
  creative1: "C418 - Biome Fest",
  creative2: "C418 - Blind Spots",
  creative3: "C418 - Haunt Muskie",
  creative4: "C418 - Aria Math",
  creative5: "C418 - Dreiton",
  creative6: "C418 - Taswell",
  nether1: "C418 - Concrete Halls",
  nether2: "C418 - Dead Voxel",
  nether3: "C418 - Warmth",
  nether4: "C418 - Ballad of the Cats",
  "music.game.end.end": "C418 - The End",
  "music.game.end.boss": "C418 - Boss",
  "music.game.end.credits": "C418 - Alpha",
  axolotl: "C418 - Axolotl",
  dragon_fish: "C418 - Dragon Fish",
  shuniji: "C418 - Shuniji",
  oxygene: "C418 - Oxygène",
  os_piano: "Amos Roddy - O's Piano",
};

async function get(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} answered HTTP ${response.status}`);
  return response.json();
}

const objectUrl = ({ hash }) => `${RESOURCES_URL}/${hash.slice(0, 2)}/${hash}`;
const assetKey = (path) => `minecraft/sounds/${path}.ogg`;
const stripNamespace = (name) => (name.includes(":") ? name.split(":")[1] : name);

function titleFor(path, lang) {
  const key = path.replaceAll("/", ".");
  if (lang[key]) return lang[key];
  const file = path.split("/").pop();
  if (KNOWN_TITLES[key] ?? KNOWN_TITLES[file]) return KNOWN_TITLES[key] ?? KNOWN_TITLES[file];
  return file
    .split("_")
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(" ");
}

// Resolves an event into [path, weight, volume] triples, following event
// references like the game's nested weighted lists.
function flatten(defs, event, volume, depth, out) {
  const def = defs[event];
  if (!def || depth > 8) return;
  for (const sound of def.sounds ?? []) {
    if (typeof sound === "string") {
      out.push([stripNamespace(sound), 1, volume]);
    } else if (sound.type === "event") {
      flatten(defs, stripNamespace(sound.name), volume * (sound.volume ?? 1), depth + 1, out);
    } else {
      out.push([stripNamespace(sound.name), sound.weight ?? 1, volume * (sound.volume ?? 1)]);
    }
  }
}

function buildCatalog(version, defs, objects, lang) {
  const catalog = { format: 1, version, events: {}, tracks: {} };
  for (const event of Object.keys(defs).filter((k) => k.startsWith("music.")).sort()) {
    const flat = [];
    flatten(defs, event, 1, 0, flat);
    const entries = [];
    for (const [path, weight, volume] of flat) {
      const object = objects[assetKey(path)];
      if (!object) continue;
      catalog.tracks[path] ??= { title: titleFor(path, lang), hash: object.hash, size: object.size };
      entries.push({ path, weight, volume: Math.round(volume * 1000) / 1000 });
    }
    if (entries.some((e) => e.weight > 0)) catalog.events[event] = entries;
  }
  if (!Object.keys(catalog.events).length) throw new Error(`Minecraft ${version} has no playable music`);
  return catalog;
}

const output = process.argv[2];
if (!output) {
  console.error("Usage: node scripts/build-catalog.mjs <output.json>");
  process.exit(2);
}

const manifest = await get(MANIFEST_URL);
const id = manifest.latest.release;
const version = await get(manifest.versions.find((v) => v.id === id).url);
const { objects } = await get(version.assetIndex.url);
const defs = await get(objectUrl(objects[SOUNDS_KEY]));
const lang = objects[LANG_KEY] ? await get(objectUrl(objects[LANG_KEY])).catch(() => ({})) : {};
const catalog = buildCatalog(id, defs, objects, lang);
await writeFile(output, JSON.stringify(catalog));
console.log(`Minecraft ${id}: ${Object.keys(catalog.events).length} situations, ${Object.keys(catalog.tracks).length} tracks`);
