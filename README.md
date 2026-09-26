# ytmdl

Search YouTube Music and download tagged tracks on an arm64 Android phone. The
app is Dioxus; the extraction is upstream yt-dlp, embedded rather than
reimplemented, so YouTube changes are fixed by updating yt-dlp from inside the
app instead of shipping a new APK.

- `crates/core`: embeds CPython 3.14 (python.org's Android build) through PyO3
  and runs the yt-dlp zipimport build with QuickJS-NG for YouTube's JS
  challenges. Search, resolve, download with progress and cancellation, M4A
  defragmenting (no ffmpeg), tagging with cover art (lofty), and the yt-dlp
  updater.
- `crates/cli`: desktop harness (`version`, `search`, `resolve`, `get`,
  `inspect`, `remux`, `update`, `selftest`).
- `crates/smoke`: arm64 Android binary that checks the native stack without the
  app.
- `app`: the Dioxus app (search, download queue, settings).

Files land in `Music/<Artist>/<Album>/<Title> [<id>].m4a` on shared storage
(after the all-files access prompt) and are added to MediaStore.

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
| Native stack on arm64 bionic under qemu-user (offline) | `tools/arm64-sysroot` once, then `tools/qemu-smoke` |
| Full app on a phone over adb | `tools/device-e2e [--build]` |
| Full app on emulated arm64 (Cuttlefish) | `tools/cf up && tools/cf wait`, `tools/device-e2e --cf` |

`tools/device-e2e` installs the APK, grants storage with `appops`, starts the
app with a test URL, and checks the downloaded file, its MediaStore row and its
tags. It needs USB debugging enabled on the phone.

Cuttlefish under QEMU emulation needs most of a 16 GB machine. Build the APK
before `tools/cf up`, never alongside it; the container is capped with
`CF_MEM_LIMIT` (default 6g).
