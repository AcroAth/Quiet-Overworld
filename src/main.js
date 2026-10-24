const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const form = $("settings");

let settings = null;
let status = null;
let detectedInstall = null;
let settingsError = null;
let versionsLoaded = false;

// [group, label] for music situations with their own names. The order here
// is the order inside each group.
const NAMED = {
  "music.game": ["Overworld", "Overworld"],
  "music.creative": ["Overworld", "Creative mode"],
  "music.game.creative": ["Overworld", "Creative mode"],
  "music.under_water": ["Overworld", "Underwater"],
  "music.game.nether": ["The Nether", "Nether"],
  "music.end": ["The End", "The End"],
  "music.game.end": ["The End", "The End"],
  "music.dragon": ["The End", "Ender Dragon fight"],
  "music.game.end.dragon": ["The End", "Ender Dragon fight"],
  "music.credits": ["The End", "End credits"],
  "music.game.end.credits": ["The End", "End credits"],
  "music.menu": ["Other", "Title screen"],
};
const GROUPS = ["Overworld", "Biomes", "The Nether", "The End", "Other"];
const ORIGINS = {
  stream: "streamed from Mojang",
  installed: "from your game files",
  folder: "from your music folder",
};

const words = (id) => id.split("_").map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(" ");

function situation(key) {
  if (NAMED[key]) return NAMED[key];
  if (key.startsWith("music.overworld.")) return ["Biomes", words(key.slice("music.overworld.".length))];
  if (key.startsWith("music.nether.")) return ["The Nether", words(key.slice("music.nether.".length))];
  return ["Other", key];
}

function clock(ms) {
  const total = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

function span(ms) {
  const seconds = ms / 1000;
  if (seconds < 90) return `${Math.round(seconds)} s`;
  const minutes = seconds / 60;
  return `${Number(minutes.toFixed(minutes % 1 ? 1 : 0))} min`;
}

function gapText([min, max]) {
  if (max === 0) return "Songs play back to back.";
  if (min === max) return `${span(min)} of quiet between songs.`;
  const [low, high] = [span(min), span(max)];
  const unit = high.split(" ")[1];
  const range = low.endsWith(unit) ? `${low.split(" ")[0]}–${high}` : `${low} to ${high}`;
  return `${range} of quiet between songs.`;
}

function renderStatus() {
  if (!status) return;
  const s = status;
  const since = Date.now() - s.atMs;
  let eyebrow = "Quiet";
  let title = " ";
  let subtitle = " ";
  let progress = false;

  if (s.state === "starting") {
    eyebrow = "Starting";
    title = "Looking for music…";
  } else if (s.state === "loading") {
    eyebrow = "Up next";
    title = s.title;
    subtitle = s.artist ?? " ";
  } else if (s.state === "playing") {
    eyebrow = s.paused ? "Paused" : "Now playing";
    title = s.title;
    subtitle = [s.artist, ORIGINS[s.origin]].filter(Boolean).join(" · ") || " ";
    const position = s.positionMs + (s.paused ? 0 : since);
    const duration = s.durationMs;
    progress = true;
    $("elapsed").textContent = clock(duration ? Math.min(position, duration) : position);
    $("total").textContent = duration ? clock(duration) : "";
    $("fill").style.width = duration ? `${Math.min(100, (position / duration) * 100)}%` : "0";
  } else if (s.paused) {
    eyebrow = "Paused";
    title = "Quiet";
    if (s.nextInMs != null) subtitle = `Next song ${clock(s.nextInMs)} after you resume`;
  } else if (s.nextInMs != null) {
    const left = s.nextInMs - since;
    title = left < 1000 ? "Any moment now…" : `Next song in ${clock(left)}`;
    subtitle = "The silence is part of it.";
  } else {
    title = "No music right now";
  }

  $("eyebrow").textContent = eyebrow;
  $("title").textContent = title;
  $("subtitle").textContent = subtitle;
  $("progress").hidden = !progress;
  const error = s.error ?? settingsError;
  $("error").hidden = !error;
  $("error").textContent = error ?? "";
  $("pause").textContent = s.paused ? "Resume" : "Pause";
  $("next").textContent = s.state === "playing" ? "Next song" : "Play a song now";
  $("library").textContent = s.library ?? " ";
  $("gap-hint").textContent = s.gapMs ? gapText(s.gapMs) : "";
}

function renderSituations() {
  if (!settings) return;
  const select = $("situation");
  const keys = [...(status?.situations ?? [])];
  if (!keys.includes(settings.situation)) keys.push(settings.situation);
  const signature = keys.join("|");
  if (select.dataset.signature !== signature) {
    select.dataset.signature = signature;
    const named = Object.keys(NAMED);
    const groups = new Map(GROUPS.map((g) => [g, []]));
    for (const key of keys) {
      const [group, label] = situation(key);
      groups.get(group).push({ key, label, rank: named.includes(key) ? named.indexOf(key) : named.length });
    }
    select.replaceChildren();
    for (const [group, items] of groups) {
      if (!items.length) continue;
      items.sort((a, b) => a.rank - b.rank || a.label.localeCompare(b.label));
      const optgroup = document.createElement("optgroup");
      optgroup.label = group;
      optgroup.append(...items.map((item) => new Option(item.label, item.key)));
      select.append(optgroup);
    }
  }
  select.value = settings.situation;
}

function ensureVersion(version) {
  const select = $("version");
  if (version && ![...select.options].some((o) => o.value === version)) {
    select.append(new Option(version, version));
  }
  select.value = version;
}

function fillForm() {
  for (const el of form.elements) {
    if (!el.name || !(el.name in settings)) continue;
    const value = settings[el.name];
    if (el.type === "checkbox") el.checked = Boolean(value);
    else if (el.name === "volume") el.value = Math.round(value * 100);
    else if (el === document.activeElement && el.type !== "select-one") continue;
    else if (el.name === "gameVersion") ensureVersion(value);
    else el.value = value ?? "";
  }
  $("volume-out").textContent = `${Math.round(settings.volume * 100)}%`;
  $("custom-row").hidden = settings.frequency !== "custom";
  const folder = settings.source === "folder";
  $("situation-row").hidden = folder;
  $("version-row").hidden = folder;
  $("install-row").hidden = folder;
  $("folder-row").hidden = !folder;
  $("minecraftDir").placeholder = detectedInstall ? `Found: ${detectedInstall}` : "Detect automatically";
  $("source-hint").textContent = sourceHint();
}

function sourceHint() {
  switch (settings.source) {
    case "local":
      return detectedInstall || settings.minecraftDir
        ? "Plays only music your game has downloaded. Never uses the network."
        : "No Minecraft Java Edition found. Launch the game once or set its folder below.";
    case "folder":
      return "Plays your own files, with the same quiet in between.";
    default:
      return detectedInstall
        ? "Uses your game's files, and streams anything missing from Mojang."
        : "Streams from Mojang's servers into memory. Nothing is saved to disk.";
  }
}

function readForm() {
  const next = { ...settings };
  for (const el of form.elements) {
    if (!el.name || !(el.name in next)) continue;
    if (el.type === "checkbox") next[el.name] = el.checked;
    else if (el.name === "volume") next.volume = Number(el.value) / 100;
    else if (el.type === "number") next[el.name] = Number(el.value);
    else next[el.name] = el.value;
  }
  return next;
}

let saving = Promise.resolve();
function save() {
  const next = readForm();
  settings = next;
  saving = saving
    .then(() => invoke("update_settings", { settings: next }))
    .then((saved) => {
      settings = saved;
      settingsError = null;
    })
    .catch((error) => {
      settingsError = String(error);
    })
    .finally(() => {
      fillForm();
      renderSituations();
      renderStatus();
    });
}

function throttle(fn, ms) {
  let last = 0;
  let timer;
  return () => {
    clearTimeout(timer);
    const wait = last + ms - Date.now();
    if (wait <= 0) {
      last = Date.now();
      fn();
    } else {
      timer = setTimeout(() => {
        last = Date.now();
        fn();
      }, wait);
    }
  };
}

async function loadVersions() {
  if (versionsLoaded) return;
  versionsLoaded = true;
  try {
    const versions = await invoke("list_versions");
    const select = $("version");
    select.replaceChildren(new Option("Latest release", ""), ...versions.map((v) => new Option(v, v)));
    ensureVersion(settings.gameVersion);
  } catch {
    versionsLoaded = false; // Offline; try again next time.
  }
}

function acceptStatus(next) {
  if (!status || next.atMs >= status.atMs) status = next;
  renderSituations();
  renderStatus();
}

async function init() {
  await listen("status", (event) => acceptStatus(event.payload));
  await listen("settings", (event) => {
    settings = event.payload;
    fillForm();
    renderSituations();
  });

  const snapshot = await invoke("snapshot");
  settings = snapshot.settings;
  detectedInstall = snapshot.detectedInstall;
  // Phones cannot have Java Edition installed, so music always streams there.
  document.body.classList.toggle("mobile", snapshot.mobile);
  fillForm();
  acceptStatus(snapshot.status);

  const saveVolume = throttle(save, 150);
  form.addEventListener("change", save);
  form.addEventListener("submit", (event) => event.preventDefault());
  form.elements.volume.addEventListener("input", (event) => {
    $("volume-out").textContent = `${event.target.value}%`;
    saveVolume();
  });

  $("pause").addEventListener("click", () => invoke("toggle_pause"));
  $("next").addEventListener("click", () => invoke("next_track"));
  $("reload").addEventListener("click", () => invoke("reload_library"));
  $("more").addEventListener("toggle", (event) => event.target.open && loadVersions());
  for (const button of document.querySelectorAll("[data-pick]")) {
    button.addEventListener("click", async () => {
      const folder = await invoke("pick_folder");
      if (folder) {
        form.elements[button.dataset.pick].value = folder;
        save();
      }
    });
  }

  setInterval(renderStatus, 500);
}

init();
