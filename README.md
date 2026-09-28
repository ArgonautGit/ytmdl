# ytmdl

A music player for an arm64 Android phone that downloads from YouTube Music:
search, download tagged tracks, and play them offline. The app is Dioxus; the
extraction is upstream yt-dlp, embedded rather than reimplemented, so YouTube
changes are fixed by updating yt-dlp from inside the app instead of shipping a
new APK.

- `crates/core`: embeds CPython 3.14 (python.org's Android build) through PyO3
  and runs the yt-dlp zipimport build with QuickJS-NG for YouTube's JS
  challenges. Search, resolve, download with progress and cancellation, M4A
  defragmenting (no ffmpeg), tagging with cover art (lofty), lyrics from
  [LRCLIB](https://lrclib.net) (time-synced where it has them, stored in the
  file as LRC), ReplayGain from the song's measured loudness (EBU R128 over
  audio decoded by symphonia), and the yt-dlp updater.
- `crates/cli`: desktop harness (`version`, `search`, `resolve`, `get`,
  `lyrics`, `inspect`, `loudness`, `remux`, `update`, `selftest`).
- `crates/smoke`: arm64 Android binary that checks the native stack without the
  app.
- `crates/library`: the library, an SQLite index (rusqlite) of downloaded
  tracks, playlists, the download queue (so it survives restarts), the saved
  play queue, listening history and saved A-B sections, plus cover art
  resized for the UI. The files stay the record: a scan of the music folders
  re-indexes them from their tags and forgets deleted ones.
- `app`: the Dioxus app (library with search and sorting, playlists, search,
  downloads, settings, listening stats, player with A-B loops over part of a
  song or a run of the queue, saved sections, a sleep timer, a reorderable
  queue and lyrics that follow the song). Songs without lyrics are looked up
  when their lyrics are opened, and what is found is saved into the file; a
  switch in Settings turns the LRCLIB lookups off. Playlists downloaded from a link stay synced with YouTube: new songs
  there are downloaded, removed ones leave the playlist (the files stay), and
  the order follows. They sync when the app starts or returns to the screen
  (at most every 30 minutes) and from the playlist's menu. yt-dlp is checked
  for updates once a day (a switch in Settings turns that off); a new build
  loads on the next start. Settings also has the open-source licenses: ytmdl's
  own and those of everything bundled with it. Songs play at an even
  loudness (a Settings switch turns that off): loud ones are turned down by
  their ReplayGain, and songs downloaded before ytmdl measured it are
  measured in the background. The app updates itself from builds published
  on GitHub (see [Published builds](#published-builds)).
- `app/android`: Kotlin driven from Rust over JNI. Playback is a Media3
  ExoPlayer service in its own `:player` process (notification, lock screen and
  headset controls), which also runs the A-B loops and the sleep timer so they
  hold with the screen off, and logs what it plays to `files/listens.log` for
  the stats, since the app may not be running. It also sets each song's
  volume from its ReplayGain. Downloads get a data-sync
  foreground service so Android doesn't freeze them in the background. The
  activity takes links shared from other apps ("Share", then ytmdl): a song
  downloads, an album or playlist opens. App updates are installed through a
  PackageInstaller session. `res/` has the launcher and
  notification icons. `tools/patch-gradle-project` adds all of it to the
  Gradle project dx generates.

Files land in `Music/<Artist>/<Album>/<Title> [<id>].m4a` on shared storage
(after the all-files access prompt) and are added to MediaStore.

The Rust side can't start twice in one process, and tao starts it again for a
second activity, so the app's process ends with its activity (music plays on
in the `:player` process), back on the root page only hides the app, and the
activity is single-task so a shared link reaches the running one.
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

Without entering the dev shell (run in the checkout):

```sh
nix run .#build-apk                # tools/build-apk; options after --
nix run .#install                  # adb install -r, building first if the source changed
nix run .#install -- --build       # always build first (build-apk options after --build)
```

After changing dependencies, run `tools/gen-notices` to refresh
`app/notices.json`, what the licenses page lists: the Rust crates built into
the app (from cargo-about) and the other bundled code in
`tools/notices/bundled.toml`. `tools/build-apk` stops while it is out of date.

Each build is numbered by the commit count (`tools/build-number`), which
becomes the APK's versionCode and shows in Settings → About.

Install keeps the app's data (library, playlists, settings). It rebuilds when
the source differs from what the APK was built from (`tools/source-stamp`,
recorded by `tools/build-apk`), whatever build options that was. The phone
needs USB debugging on; with several devices attached, pick one with
`ANDROID_SERIAL`.

## Published builds

`.github/workflows/apk.yml` builds the APK on GitHub (in `nix develop .#ci`,
the dev shell with only what the APK build needs) and publishes it as the
release `build-<n>`, with `ytmdl-update.json` describing it. Run it from the
Actions tab (APK → Run workflow, for any branch) or with:

```sh
gh workflow run apk.yml                     # build main and publish it
gh workflow run apk.yml --ref my-branch -f publish=false   # just build; the APK is a workflow artifact
```

The app checks the newest release once a day (Settings → App, where the
switch turns that off and "Check for updates" checks now) and downloads a
build numbered higher than its own. Installing waits for the "Install build
<n>" button, since it closes the app and stops playback. The first time,
Android asks to allow ytmdl to install apps; after that, on Android 12 and
later, updates install without asking. The library and settings stay, and
the first start of the new build says "Updated to build <n>".

Android installs an update only when it is signed with the same key as the
installed app, and the APK is debug-signed, so publishing needs the debug
keystore your builds use as a repository secret. Once, with the GitHub CLI
(run `tools/build-apk` first if you have never built, which creates the
keystore):

```sh
f="${XDG_DATA_HOME:-$HOME/.local/share}/android-nix/debug.keystore"   # $ANDROID_USER_HOME in the dev shell
test -s "$f" && base64 < "$f" | tr -d '\n' | gh secret set ANDROID_DEBUG_KEYSTORE
gh secret list   # ANDROID_DEBUG_KEYSTORE, updated just now
keytool -list -v -keystore "$f" -storepass android | grep SHA256   # in the dev shell
```

`test -s` keeps a missing keystore from being stored as an empty secret, which
the workflow can't tell from no secret at all. Or paste that base64 as a
repository secret under Settings → Secrets and variables → Actions; GitHub
never shows a secret's value again, so its edit page looks empty even when it
isn't. The workflow logs the key's SHA-256 fingerprint, which must match the
one `keytool` shows for your keystore, the key of the app on the phone.

Published builds and your own then update one another; `nix run .#install`
also installs a build older than the phone's (`adb install -d`). A phone with
an app signed by another key has to uninstall it once (losing the library
index and playlists; the music files stay and are scanned again).

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

## License

ytmdl is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. It comes with no warranty. See [LICENSE](LICENSE).

The APK also carries third-party code under its own licenses: CPython, yt-dlp,
QuickJS-NG, OpenSSL and other libraries, the AndroidX and Kotlin libraries, and
the Rust crates. Settings → Open-source licenses lists each with its license
text.
