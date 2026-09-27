# ytmdl

A music player for an arm64 Android phone that downloads from YouTube Music:
search, download tagged tracks, and play them offline. The app is Dioxus; the
extraction is upstream yt-dlp, embedded rather than reimplemented, so YouTube
changes are fixed by updating yt-dlp from inside the app instead of shipping a
new APK.

- `crates/core`: embeds CPython 3.14 (python.org's Android build) through PyO3
  and runs the yt-dlp zipimport build with QuickJS-NG for YouTube's JS
  challenges. Search, resolve, download with progress and cancellation, M4A
  defragmenting (no ffmpeg), tagging with cover art (lofty), and the yt-dlp
  updater.
- `crates/cli`: desktop harness (`version`, `search`, `resolve`, `get`,
  `inspect`, `remux`, `update`, `selftest`).
- `crates/smoke`: arm64 Android binary that checks the native stack without the
  app.
- `crates/library`: the library, an SQLite index (rusqlite) of downloaded
  tracks, playlists, the download queue (so it survives restarts) and the
  saved play queue, plus cover art resized for the UI. The files stay the
  record: a scan of the music folders re-indexes them from their tags and
  forgets deleted ones.
- `app`: the Dioxus app (library, playlists, search, downloads, settings,
  player with A-B loops over part of a song or a run of the queue).
- `app/android`: Kotlin driven from Rust over JNI. Playback is a Media3
  ExoPlayer service in its own `:player` process (notification, lock screen and
  headset controls), which also runs the A-B loops so they hold with the screen
  off. Downloads get a data-sync foreground service so Android doesn't freeze
  them in the background. `res/` has the launcher and notification icons.
  `tools/patch-gradle-project` adds all of it to the Gradle project dx
  generates.

Files land in `Music/<Artist>/<Album>/<Title> [<id>].m4a` on shared storage
(after the all-files access prompt) and are added to MediaStore.

The Rust side can't start twice in one process, and tao starts it again for a
second activity, so the app's process ends with its activity (music plays on
in the `:player` process) and back on the root page only hides the app.
Downloads live in the app's process, so swiping the app away stops them; they
pick up again on the next start.

## Setup

```sh
nix develop            # SDK, NDK, Rust, dx, qemu; accepts the Android SDK license
tools/fetch-deps       # python.org CPython for Android + yt-dlp, checked against tools/deps.sha256
tools/build-qjs        # QuickJS-NG for arm64-v8a
```

## Build

```sh
tools/build-apk        # -> target/ytmdl-arm64.apk (release Rust, debug-signed, autotest hook)
cargo run -p ytmdl-cli -- get <url> -o ~/Music     # desktop, same core
```

## Test

| What | Command |
|---|---|
| Unit tests | `cargo test` |
| Every screen as static HTML with sample data | `tools/ui-preview [--sample QUERY]`, then open `target/ui-preview/index.html` |
| Native stack on arm64 bionic under qemu-user (offline) | `tools/arm64-sysroot` once, then `tools/qemu-smoke` |
| Full app on a phone over adb | `tools/device-e2e [--build]` |
| Full app on emulated arm64 (Cuttlefish) | `tools/cf up && tools/cf wait`, `tools/device-e2e --cf` |

`tools/device-e2e` installs the APK, grants storage with `appops`, starts the
app with a test URL, and checks the downloaded file, its MediaStore row and its
tags. It needs USB debugging enabled on the phone.

Cuttlefish under QEMU emulation needs most of a 16 GB machine. Build the APK
before `tools/cf up`, never alongside it; the container is capped with
`CF_MEM_LIMIT` (default 6g).
