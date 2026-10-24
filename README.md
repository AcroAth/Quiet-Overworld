# Quiet Overworld

Minecraft with only the music part. A small app that plays Minecraft's background music the way the game does: a random
track from the overworld soundtrack, then ten to twenty minutes of quiet, then another. It lives in the system tray on
Windows, macOS and Linux, and keeps playing in the background on Android and iOS.

![Quiet Overworld, playing and between songs](docs/screenshot.png)

## Where the music comes from

The app ships with **no music**, and never saves any to disk. It finds the music the same way the game's own launcher
does:

1. **Your installed game.** If Minecraft: Java Edition is installed, tracks are played straight from its files. The
   official launcher's and Prism Launcher's folders are found automatically; others can be set under More settings.
2. **Mojang's servers.** Anything not installed is streamed from Mojang's official asset server
   (`resources.download.minecraft.net`) into memory, played, and dropped.
3. **Your own files.** Bought the soundtrack? Point "My own music folder" at it (`.ogg`, `.mp3`, `.flac`, `.wav`) and the
   app plays it with the same quiet in between.

Which tracks belong to which situation, their relative volumes and their titles are read from the game's own
`sounds.json` and language file. They are the same as in the game, and new tracks from game updates appear
automatically. Only that metadata (a few kB) is cached.

Minecraft's music is © Mojang Studios and its composers (C418, Lena Raine, Kumi Tanioka, Aaron Cherof, Amos Roddy,
fingerspit). This project is not affiliated with Mojang or Microsoft. It is meant for people who own the game; the
"Installed game only" source never touches the network at all.

## How it plays

The defaults copy the game exactly (`MusicManager` in Java Edition):

| | Game rule | In the app |
| --- | --- | --- |
| Music | Overworld survival music (`music.game`) in most biomes | **Overworld** (default) |
| First song | 5 seconds after start | 5 seconds after launch |
| Between songs | random 10–20 minutes in the overworld | same |
| Track choice | weighted random from the situation's list | same weights and per-track volumes |
| Music Frequency option | Default, Frequent (≤ 10 min), Constant (5 s) | same, plus Custom (your own range) |
| Changing situation | title screen and End music cut the current song; others let it finish | same |

Other situations can be picked from the **Music** menu: Creative mode, Underwater, every biome with its own music
(Meadow, Cherry Grove, Deep Dark, …), the Nether biomes, the End, the dragon fight, the credits and the title screen.
A specific game version can be chosen under **More settings** (for example an older release with only C418's music).

Controls: **Pause** freezes the song or the countdown. **Play a song now** / **Next song** skips the wait or the current
track.

## Light on resources

- No music, browser engine or runtime is bundled. The app uses the OS webview (Tauri): the Linux installer is under
  4 MB, about 9 MB once installed.
- Closing the window **destroys** the webview instead of hiding it. In the tray only the small Rust process is left.
  Started with `--hidden` (as autostart does), it runs with no browser processes at all: about 12 MB of private memory
  in one process on Linux.
- Between songs nothing runs: no polling and no timers ticking 20 times a second. The engine sleeps until the next
  deadline. Measured on Linux: 0 CPU ticks over 30 s of quiet.
- On desktop the audio device is opened only while a song plays, and released between songs and while paused, so the
  app never keeps your computer awake.
- A streamed track (3–8 MB) is held in memory only while it plays.

## Platforms

| | Background | Notes |
| --- | --- | --- |
| Windows, macOS, Linux | System tray: play/pause, next, open, quit | Closing the window keeps the music going (can be turned off). Optional start at login. On macOS the Dock icon hides while only the tray icon is left. |
| Android 8+ | Foreground "playing music" notification with a Quit button | Keeps playing with the screen off. Pausing lets Android stop the app as usual. |
| iOS | Background audio | Mixes with other apps' audio instead of stopping it. |

On phones the music always streams from Mojang (Java Edition is not installable there).

## Install

CI builds installers on every push: open the latest successful [Build workflow
run](https://github.com/AcroAth/Quiet-Overworld/actions/workflows/build.yml) and download the artifact for your system
(`.msi`/`.exe`, `.dmg`, `.deb`/`.rpm`/`.AppImage`, `.apk`).

- The builds are not signed by a known developer. Windows SmartScreen warns the first time (More info → Run anyway).
  On macOS, open the app once, then allow it under System Settings → Privacy & Security → Open Anyway (or run
  `xattr -dr com.apple.quarantine "/Applications/Quiet Overworld.app"`).
- On Linux prefer the `.deb` or `.rpm` (under 4 MB). The `.AppImage` carries its own copy of the web engine and is
  around 80 MB. The tray needs `libayatana-appindicator3-1` (installed with the `.deb`). Most panels open the tray menu
  on click; use **Open Quiet Overworld** there.
- CI signs the Android APK with a throwaway key unless the repository secrets `ANDROID_KEYSTORE_BASE64`,
  `ANDROID_KEYSTORE_PASSWORD` and `ANDROID_KEY_ALIAS` are set. Installing an update over an APK signed with another
  key fails; uninstall first.

## Build from source

You need [Rust](https://rustup.rs) (1.90+), Node.js 18+ and the [Tauri
prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS. On Debian/Ubuntu:

```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev libasound2-dev
```

Then:

```sh
npm install
npm run tauri dev      # run it
npm run tauri build    # installers in src-tauri/target/release/bundle
```

**Android** (Android Studio SDK + NDK, `NDK_HOME` set, `rustup target add aarch64-linux-android armv7-linux-androideabi`):

```sh
npm run tauri android init
npm run tauri android dev          # on a connected phone or emulator
npm run tauri android build -- --apk
```

**iOS** (macOS with Xcode, `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`). Building for a real iPhone
needs an Apple developer team (`bundle > iOS > developmentTeam` in `tauri.conf.json`, or `APPLE_DEVELOPMENT_TEAM`):

```sh
npm run tauri ios init
npm run tauri ios dev
```

## How it's built

```
src/                     The window: plain HTML, CSS and JavaScript, no framework or bundler
src-tauri/src/
  catalog.rs             sounds.json → music situations, weights, volumes, titles
  sources.rs             Mojang's version manifest and asset server, installed game folders, music folders
  library.rs             picks the next track and fetches it; caches the catalog (metadata only)
  settings.rs            settings and the game's timing rules (Musics, MusicFrequency)
  engine.rs              the MusicManager port: deadlines, pause, skip, situation changes
  audio.rs               playback with rodio; opens the device only while needed
  tray.rs                tray icon, window lifecycle, autostart (desktop)
  mobile.rs              background playback hooks (phones)
src-tauri/plugins/background-audio/
                         Android foreground service (Kotlin) and iOS audio session
scripts/make_icon.py     draws the pixel-art icon
```

Settings live in the OS config folder (`settings.json`, e.g. `~/.config/io.github.acroath.quietoverworld/`), and the
cached catalog in the OS cache folder.

### Testing

```sh
cd src-tauri && cargo test
```

To try the whole app without Mojang's servers, point it at a local mirror with the same layout:

```sh
QUIET_OVERWORLD_MANIFEST_URL=http://127.0.0.1:8765/manifest.json \
QUIET_OVERWORLD_RESOURCES_URL=http://127.0.0.1:8765/res \
npm run tauri dev
```

## Not done yet

- No lock-screen or media-key controls (pause and next are in the app and the desktop tray).
- No volume ducking for notifications or calls; Android and iOS mix the music with other audio.
- iOS builds are only checked in CI for the simulator; installing on an iPhone needs your own signing.
