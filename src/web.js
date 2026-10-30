// The web version's stand-in for the Rust backend: the same commands and
// events, with the game's MusicManager timing ported from
// src-tauri/src/engine.rs and settings.rs.
//
// Everything plays through one <audio> element. Phones freeze a page once it
// stops playing sound, so the silence between songs is real (silent) audio on
// that element, and the deadline is checked from its timeupdate events, which
// keep coming with the screen locked. The music streams from Mojang's asset
// server; catalog.json (track names and hashes only) is built by CI with
// scripts/build-catalog.mjs.

const RESOURCES_URL = "https://resources.download.minecraft.net";
// Mojang labels its files as plain data, which Safari's engine (every browser
// on an iPhone) refuses to play. relay/worker.js passes them on labelled as
// audio; it is used only once a browser has refused a direct link.
const RELAY_URL = "";
const SETTINGS_KEY = "quiet-overworld.settings";
const DEFAULT_SITUATION = "music.game";
const TICK_MS = 50;
const STARTING_DELAY_MS = 5000;

const DEFAULTS = {
  situation: DEFAULT_SITUATION,
  frequency: "default",
  customMinMinutes: 3,
  customMaxMinutes: 8,
  volume: 1,
  source: "minecraft",
  gameVersion: "",
  minecraftDir: "",
  musicFolder: "",
  closeToTray: false,
  startHidden: false,
  autostart: false,
};

// Delays of each kind of music in ticks: [min, max, replaces the current song].
function timing(situation) {
  switch (situation) {
    case "music.menu":
      return [20, 600, true];
    case "music.end":
    case "music.game.end":
      return [6000, 24000, true];
    case "music.dragon":
    case "music.game.end.dragon":
    case "music.credits":
    case "music.game.end.credits":
      return [0, 0, true];
    default:
      return [12000, 24000, false];
  }
}

function delayRange(s) {
  const [min, max] = timing(s.situation);
  const capped = (cap) => [TICK_MS * Math.min(min, cap), TICK_MS * Math.min(max, cap)];
  switch (s.frequency) {
    case "frequent":
      return capped(12000);
    case "constant":
      return [TICK_MS * 100, TICK_MS * 100];
    case "custom":
      return [s.customMinMinutes * 60000, s.customMaxMinutes * 60000];
    default:
      return capped(24000);
  }
}

const randomInt = (min, max) => min + Math.floor(Math.random() * (max - min + 1));

function sanitized(input) {
  const s = { ...DEFAULTS, ...input };
  if (typeof s.situation !== "string" || !s.situation.trim()) s.situation = DEFAULT_SITUATION;
  if (!["default", "frequent", "constant", "custom"].includes(s.frequency)) s.frequency = "default";
  s.volume = Number.isFinite(s.volume) ? Math.min(1, Math.max(0, s.volume)) : 1;
  const minutes = (m, fallback) => (Number.isFinite(m) ? Math.min(1440, Math.max(0, m)) : fallback);
  s.customMinMinutes = minutes(s.customMinMinutes, 3);
  s.customMaxMinutes = Math.max(minutes(s.customMaxMinutes, 8), s.customMinMinutes);
  return s;
}

function loadSettings() {
  try {
    return sanitized(JSON.parse(localStorage.getItem(SETTINGS_KEY)) ?? {});
  } catch {
    return sanitized({});
  }
}

function storeSettings(s) {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(s));
  } catch {
    // Private browsing: settings last until the page is closed.
  }
}

// One second of silent 16-bit mono WAV, looped during the quiet.
function silenceUrl() {
  const rate = 8000;
  const bytes = rate * 2;
  const view = new DataView(new ArrayBuffer(44 + bytes));
  const text = (offset, s) => [...s].forEach((c, i) => view.setUint8(offset + i, c.charCodeAt(0)));
  text(0, "RIFF");
  view.setUint32(4, 36 + bytes, true);
  text(8, "WAVE");
  text(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, bytes, true);
  return URL.createObjectURL(new Blob([view], { type: "audio/wav" }));
}

// iPhones ignore volume set by a page; the system buttons control it.
function volumeWorks() {
  const probe = document.createElement("audio");
  probe.volume = 0.5;
  return probe.volume === 0.5;
}

function pick(catalog, situation, avoid) {
  const entries = catalog.events[situation];
  if (!entries) return null;
  let pool = entries.filter((e) => e.path !== avoid);
  if (pool.every((e) => e.weight === 0)) pool = entries;
  const total = pool.reduce((sum, e) => sum + e.weight, 0);
  if (!total) return null;
  let roll = Math.random() * total;
  for (const entry of pool) {
    if (roll < entry.weight) return entry;
    roll -= entry.weight;
  }
  return pool[pool.length - 1];
}

function trackCount(catalog, situation) {
  return new Set((catalog.events[situation] ?? []).map((e) => e.path)).size;
}

function splitTitle(full) {
  const at = full.indexOf(" - ");
  return at < 0 ? [full, null] : [full.slice(at + 3), full.slice(0, at)];
}

const listeners = { status: [], settings: [] };
const emit = (event, payload) => listeners[event].forEach((fn) => fn({ payload }));

const audio = document.createElement("audio");
audio.preload = "auto";
audio.setAttribute("playsinline", "");
const silence = silenceUrl();

const engine = {
  settings: loadSettings(),
  catalog: null,
  catalogError: null,
  catalogFailures: 0,
  catalogRetry: null,
  // Nothing plays until the first tap: browsers only allow sound after one.
  started: false,
  paused: true,
  // "waiting" | "loading" | "playing". While waiting, `due` is when the next
  // song starts; it is null while paused or blocked, and `left` then holds the
  // remaining time.
  phase: "waiting",
  due: null,
  left: STARTING_DELAY_MS,
  track: null,
  trackError: null,
  trackFailures: 0,
  // Set while the engine itself pauses the element, so a pause coming from
  // the system (a call, another app's audio) can be told apart.
  pausing: false,
  useRelay: false,
};

async function loadCatalog() {
  clearTimeout(engine.catalogRetry);
  try {
    const response = await fetch("catalog.json", { cache: "no-cache" });
    if (!response.ok) throw new Error(`The track list could not be loaded (HTTP ${response.status})`);
    engine.catalog = await response.json();
    engine.catalogError = null;
    engine.catalogFailures = 0;
  } catch (error) {
    engine.catalogFailures += 1;
    const retry = Math.min(30000 * 2 ** Math.min(engine.catalogFailures - 1, 5), 30 * 60000);
    engine.catalogRetry = setTimeout(loadCatalog, retry);
    if (!engine.catalog) engine.catalogError = navigator.onLine ? String(error.message ?? error) : "You are offline";
  }
  if (engine.catalog && engine.phase === "waiting" && engine.due === null && !engine.paused) {
    engine.due = Date.now() + engine.left;
  }
  publish();
}

function play() {
  engine.pausing = false;
  audio.play().catch((error) => {
    if (error.name === "NotAllowedError") setPaused(true);
  });
}

function stopElement() {
  if (audio.paused) return;
  engine.pausing = true;
  audio.pause();
}

// Enters the silence before the next song.
function wait(delay) {
  engine.phase = "waiting";
  engine.track = null;
  engine.left = delay;
  engine.due = engine.paused ? null : Date.now() + delay;
  audio.loop = true;
  if (audio.src !== silence) audio.src = silence;
  if (!engine.paused) play();
  publish();
}

function startTrack(avoid) {
  if (!engine.catalog) return wait(0);
  const entry = pick(engine.catalog, engine.settings.situation, avoid);
  const info = entry && engine.catalog.tracks[entry.path];
  if (!info) {
    engine.trackError = "There is no music for this situation here, pick another one";
    wait(0);
    engine.due = null;
    return publish();
  }
  const [title, artist] = splitTitle(info.title);
  engine.phase = "loading";
  engine.due = null;
  engine.track = { key: entry.path, title, artist, gain: entry.volume, hash: info.hash };
  audio.loop = false;
  audio.src = trackUrl(info.hash);
  applyVolume();
  if (!engine.paused) play();
  publish();
}

function trackUrl(hash) {
  return engine.useRelay ? `${RELAY_URL}/${hash}` : `${RESOURCES_URL}/${hash.slice(0, 2)}/${hash}`;
}

function finishTrack() {
  const [min, max] = delayRange(engine.settings);
  wait(randomInt(min, max));
}

function failTrack(message) {
  engine.trackFailures += 1;
  engine.trackError = message;
  wait(Math.min(15000 * 2 ** Math.min(engine.trackFailures - 1, 5), 600000));
}

function applyVolume() {
  audio.volume = Math.min(1, engine.settings.volume * (engine.track?.gain ?? 1));
}

function setPaused(paused) {
  if (engine.paused === paused) return;
  engine.paused = paused;
  if (engine.phase === "waiting") {
    if (paused) {
      if (engine.due !== null) engine.left = Math.max(0, engine.due - Date.now());
      engine.due = null;
    } else if (engine.catalog) {
      engine.due = Date.now() + engine.left;
    }
  }
  if (paused) stopElement();
  else play();
  publish();
}

// Called from a tap. The first one unlocks sound for the page.
function userStart() {
  if (engine.started) return false;
  engine.started = true;
  wait(engine.left);
  setPaused(false);
  return true;
}

function next() {
  if (!engine.started) engine.started = true;
  const avoid = engine.track?.key;
  engine.paused = false;
  startTrack(avoid);
}

function applySettings(next) {
  const old = engine.settings;
  engine.settings = next;
  applyVolume();
  const timingChanged = ["situation", "frequency", "customMinMinutes", "customMaxMinutes"].some(
    (key) => old[key] !== next[key],
  );
  if (!timingChanged) return publish();
  engine.trackError = null;
  const [minTicks, , replaces] = timing(next.situation);
  const max = delayRange(next)[1];
  if (engine.phase !== "waiting" && replaces && old.situation !== next.situation) {
    wait(TICK_MS * randomInt(0, Math.floor(minTicks / 2)));
  } else if (engine.phase === "waiting") {
    if (engine.due !== null) engine.due = Math.min(engine.due, Date.now() + max);
    else {
      engine.left = Math.min(engine.left, max);
      if (!engine.paused && engine.started && engine.catalog) engine.due = Date.now() + engine.left;
    }
  }
  publish();
}

function checkDeadline() {
  if (engine.phase === "waiting" && engine.due !== null && engine.catalog && Date.now() >= engine.due) {
    startTrack(null);
  }
}

audio.addEventListener("timeupdate", checkDeadline);
audio.addEventListener("playing", () => {
  if (engine.phase === "loading") {
    engine.phase = "playing";
    engine.trackError = null;
    engine.trackFailures = 0;
  }
  publish();
});
audio.addEventListener("loadedmetadata", publish);
audio.addEventListener("ended", () => {
  if (engine.phase === "playing" || engine.phase === "loading") finishTrack();
});
audio.addEventListener("error", () => {
  if (engine.phase !== "loading" && engine.phase !== "playing") return;
  const title = engine.track?.title ?? "The song";
  const unsupported = audio.error?.code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED;
  if (unsupported && RELAY_URL && !engine.useRelay) {
    engine.useRelay = true;
    audio.src = trackUrl(engine.track.hash);
    if (!engine.paused) play();
    return;
  }
  failTrack(
    unsupported
      ? `This browser cannot play Minecraft's music files (Ogg Vorbis)`
      : `"${title}" could not be streamed from Mojang, will try again`,
  );
});
// A call or another app's audio pauses the element: treat it as a pause.
audio.addEventListener("pause", () => {
  // Switching tracks also pauses the element for a moment; by the time the
  // event arrives it is playing again.
  if (engine.pausing || audio.ended || !audio.paused) return;
  setPaused(true);
});

if ("mediaSession" in navigator) {
  const session = navigator.mediaSession;
  const handle = (action, fn) => {
    try {
      session.setActionHandler(action, fn);
    } catch {
      // Not supported here.
    }
  };
  handle("play", () => userStart() || setPaused(false));
  handle("pause", () => setPaused(true));
  handle("nexttrack", next);
  handle("seekbackward", null);
  handle("seekforward", null);
}

function updateMediaSession(status) {
  if (!("mediaSession" in navigator) || typeof MediaMetadata === "undefined") return;
  const playing = status.state === "playing" || status.state === "loading";
  const title = playing ? status.title : "Quiet";
  const artist = playing ? (status.artist ?? "") : "The next song comes on its own";
  const current = navigator.mediaSession.metadata;
  if (current?.title !== title || current?.artist !== artist) {
    navigator.mediaSession.metadata = new MediaMetadata({
      title,
      artist,
      album: "Quiet Overworld",
      artwork: [{ src: new URL("icon.png", location.href).href, sizes: "64x64", type: "image/png" }],
    });
  }
  navigator.mediaSession.playbackState = engine.paused ? "paused" : "playing";
}

function status() {
  const now = Date.now();
  const catalog = engine.catalog;
  const s = {
    state: "waiting",
    paused: engine.paused,
    title: null,
    artist: null,
    origin: "stream",
    positionMs: 0,
    durationMs: null,
    nextInMs: null,
    gapMs: delayRange(engine.settings),
    atMs: now,
    library: null,
    situations: catalog ? Object.keys(catalog.events) : [],
    error: engine.trackError ?? engine.catalogError,
  };
  if (catalog) {
    const count = trackCount(catalog, engine.settings.situation);
    s.library = `Minecraft ${catalog.version} · ${count} ${count === 1 ? "track" : "tracks"}`;
  }
  if (!engine.started) {
    s.state = "ready";
  } else if (engine.phase === "playing" || engine.phase === "loading") {
    s.state = engine.phase;
    s.title = engine.track.title;
    s.artist = engine.track.artist;
    s.positionMs = Math.round(audio.currentTime * 1000);
    s.durationMs = Number.isFinite(audio.duration) ? Math.round(audio.duration * 1000) : null;
  } else if (!catalog && !engine.catalogError) {
    s.state = "starting";
  } else if (engine.due !== null) {
    s.nextInMs = Math.max(0, engine.due - now);
  } else if (engine.paused) {
    s.nextInMs = engine.left;
  }
  return s;
}

function publish() {
  const s = status();
  updateMediaSession(s);
  emit("status", s);
}

// Also checks while the page is in front, e.g. if the silent loop stalls.
setInterval(checkDeadline, 1000);
loadCatalog();

export async function listen(event, callback) {
  listeners[event]?.push(callback);
}

export async function invoke(command, args) {
  switch (command) {
    case "snapshot":
      return {
        settings: engine.settings,
        status: status(),
        mobile: true,
        web: true,
        fixedVolume: !volumeWorks(),
        detectedInstall: null,
      };
    case "update_settings": {
      const next = sanitized(args.settings);
      storeSettings(next);
      applySettings(next);
      emit("settings", next);
      return next;
    }
    case "toggle_pause":
      if (!userStart()) setPaused(!engine.paused);
      return;
    case "next_track":
      return next();
    case "reload_library":
      return loadCatalog();
    case "list_versions":
      return [];
    case "pick_folder":
      return null;
    default:
      throw new Error(`Unknown command ${command}`);
  }
}
