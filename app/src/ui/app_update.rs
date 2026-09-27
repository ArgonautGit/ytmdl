//! Updates of the app itself. .github/workflows/apk.yml builds the APK on
//! GitHub and publishes it as a release, with `ytmdl-update.json` describing
//! it; the newest release's copy says which build is out. Once a day (unless
//! that is turned off) the app reads it and downloads a build newer than its
//! own. Installing one ends the app, and playback with it, so that waits for
//! the Install button in Settings. The first start of the new build says so.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use dioxus::prelude::*;
use serde::Deserialize;
use ytmdl_core::Runtime;
use ytmdl_library::Library;

use super::Ctx;
use super::sync::{ago, unix_now};
use crate::platform;

/// The newest published build's description.
const MANIFEST_URL: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases/latest/download/ytmdl-update.json");
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Far above an APK's size; a bigger one is refused.
const MAX_APK_BYTES: u64 = 512 * 1024 * 1024;

const CHECK_EVERY_SECS: i64 = 24 * 3600;
/// A failed automatic check is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(3600);

const AUTO: &str = "app_update_auto";
const CHECKED: &str = "app_update_checked_at";
/// The build that ran last, to notice that an update was installed.
const LAST_BUILD: &str = "app_last_build";

/// This build's number (tools/build-number), which tools/build-apk compiles in.
pub fn this_build() -> Option<u32> {
    option_env!("YTMDL_BUILD")?.parse().ok()
}

/// "0.1.0 • build 57 • 1a2b3c4", as far as this build knows, for About.
pub fn version_text() -> String {
    let mut parts = vec![env!("CARGO_PKG_VERSION").to_owned()];
    if let Some(build) = this_build() {
        parts.push(format!("build {build}"));
    }
    if let Some(commit) = option_env!("YTMDL_COMMIT") {
        parts.push(short_commit(commit));
    }
    parts.join(" • ")
}

fn short_commit(commit: &str) -> String {
    let (sha, dirty) = match commit.strip_suffix("-dirty") {
        Some(sha) => (sha, true),
        None => (commit, false),
    };
    let short = sha.get(..7).unwrap_or(sha);
    if dirty { format!("{short}, modified") } else { short.to_owned() }
}

/// `ytmdl-update.json`, as .github/workflows/apk.yml writes it.
#[derive(Deserialize, Debug, Clone, PartialEq)]
struct Release {
    build: u32,
    url: String,
    sha256: String,
    size: u64,
}

/// A newer build, downloaded and checked, waiting to be installed.
#[derive(Clone, PartialEq, Debug)]
pub struct Downloaded {
    pub build: u32,
    pub path: PathBuf,
}

#[derive(Clone, PartialEq, Debug)]
pub struct AppUpdates {
    /// Daily checks are on.
    pub auto: bool,
    /// Unix seconds of the last check that got an answer.
    pub checked_at: Option<i64>,
    /// A check or download is running.
    pub busy: bool,
    pub ready: Option<Downloaded>,
    /// The last check's or install's outcome, for Settings.
    pub message: Option<String>,
    /// The last automatic attempt this run.
    tried: Option<Instant>,
    /// Where downloaded builds wait.
    dir: PathBuf,
}

impl AppUpdates {
    /// Settings, and a build downloaded on an earlier run; `dir` holds downloads.
    pub fn load(library: &Library, dir: PathBuf) -> AppUpdates {
        let get = |key| library.setting(key).ok().flatten();
        AppUpdates {
            auto: get(AUTO).as_deref() != Some("0"),
            checked_at: get(CHECKED).and_then(|s| s.parse().ok()),
            busy: false,
            ready: this_build().and_then(|build| waiting(&dir, build)),
            message: None,
            tried: None,
            dir,
        }
    }

    /// Under the Settings switch.
    pub fn note(&self) -> String {
        if !self.auto {
            return "Off: check with the button above".into();
        }
        let checked = self.checked_at.map(|t| format!(" · last checked {}", ago(unix_now() - t)));
        format!("Checks daily and downloads new builds{}", checked.unwrap_or_default())
    }
}

/// The newest build in `dir` newer than `current`; everything else there
/// (older or installed builds, unfinished downloads) is deleted.
fn waiting(dir: &Path, current: u32) -> Option<Downloaded> {
    let mut best: Option<Downloaded> = None;
    let mut stale = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        match apk_build(&path) {
            Some(build) if build > current && best.as_ref().is_none_or(|b| build > b.build) => {
                stale.extend(best.replace(Downloaded { build, path }).map(|b| b.path));
            }
            _ => stale.push(path),
        }
    }
    for path in stale {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(target: "ytmdl", "removing {}: {e}", path.display());
        }
    }
    best
}

fn apk_name(build: u32) -> String {
    format!("ytmdl-{build}.apk")
}

fn apk_build(path: &Path) -> Option<u32> {
    path.file_name()?.to_str()?.strip_prefix("ytmdl-")?.strip_suffix(".apk")?.parse().ok()
}

/// What a check found.
enum Found {
    /// No build is published yet.
    Nothing,
    /// The newest published build is this one, or older.
    NotNewer(u32),
    /// A newer build is downloaded: by this check, or before it.
    Ready { downloaded: Downloaded, now: bool },
}

/// Reads which build is published and downloads it when it is newer than
/// `current` and not already downloaded.
async fn check(
    rt: Runtime,
    current: u32,
    ready: Option<Downloaded>,
    dir: PathBuf,
    mut updates: Signal<AppUpdates>,
) -> anyhow::Result<Found> {
    let Some(body) = rt.fetch_optional(MANIFEST_URL.into(), MAX_MANIFEST_BYTES, Vec::new()).await? else {
        return Ok(Found::Nothing);
    };
    let release: Release = serde_json::from_slice(&body).context("reading ytmdl-update.json")?;
    if release.build <= current {
        return Ok(Found::NotNewer(release.build));
    }
    if let Some(downloaded) = ready.filter(|r| r.build >= release.build && r.path.is_file()) {
        return Ok(Found::Ready { downloaded, now: false });
    }
    if release.size > MAX_APK_BYTES {
        bail!("build {} is too big ({})", release.build, megabytes(release.size));
    }
    updates.write().message = Some(format!("Downloading build {} ({})…", release.build, megabytes(release.size)));
    std::fs::create_dir_all(&dir)?;
    let part = dir.join(format!("{}.part", apk_name(release.build)));
    let path = dir.join(apk_name(release.build));
    let saved = match rt.download_to(release.url.clone(), part.clone(), release.size).await {
        Ok(sha256) if sha256.eq_ignore_ascii_case(&release.sha256) => std::fs::rename(&part, &path).map_err(Into::into),
        Ok(_) => Err(anyhow!("the download of build {} is damaged (checksum mismatch)", release.build)),
        Err(e) => Err(e.into()),
    };
    if saved.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    saved?;
    Ok(Found::Ready { downloaded: Downloaded { build: release.build, path }, now: true })
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

/// PackageInstaller's STATUS_FAILURE_* codes, in words.
fn install_failure(status: i32, message: &str) -> String {
    match status {
        2 => "Android blocked the update.".into(),
        3 => "Update cancelled.".into(),
        5 => "Android won't install the update over this app: it's signed with a different key. \
              Published builds and yours must share one debug key (README, “Published builds”)."
            .into(),
        6 => "Not enough storage to install the update.".into(),
        _ => format!("Couldn't install the update: {message}"),
    }
}

impl Ctx {
    /// Looks for a newer build and downloads it. `manual` (from Settings)
    /// reports even when nothing changed.
    pub(super) fn check_app_update(&self, manual: bool) {
        let Some(svc) = self.services() else { return };
        let Some(current) = this_build().filter(|_| platform::app_update::SUPPORTED) else { return };
        let mut updates = self.app_updates;
        if updates.peek().busy {
            return;
        }
        let (ready, dir) = {
            let mut u = updates.write();
            u.busy = true;
            if manual {
                u.message = Some("Checking…".into());
            } else {
                u.tried = Some(Instant::now());
            }
            (u.ready.clone(), u.dir.clone())
        };
        let ctx = *self;
        let library = self.library.get();
        let rt = svc.dl.runtime().clone();
        spawn(async move {
            let result = check(rt, current, ready, dir.clone(), updates).await;
            let message = match &result {
                Ok(Found::Nothing) => "No builds are published yet.".to_owned(),
                Ok(Found::NotNewer(published)) if *published == current => format!("Build {current} is the newest."),
                Ok(Found::NotNewer(published)) => {
                    format!("This build ({current}) is newer than the published one ({published}).")
                }
                Ok(Found::Ready { downloaded, now }) => {
                    if *now && !manual {
                        ctx.notify(format!("ytmdl build {} is downloaded. Install it in Settings.", downloaded.build));
                    }
                    format!("Build {} is downloaded. Installing it closes the app.", downloaded.build)
                }
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "app update check: {e:#}");
                    // Python errors carry a traceback after the first line.
                    let e = format!("{e:#}");
                    format!("Update failed: {}", e.lines().next().unwrap_or_default())
                }
            };
            let now = unix_now();
            if result.is_ok()
                && let Err(e) = library.set_setting(CHECKED, &now.to_string())
            {
                tracing::warn!(target: "ytmdl", "saving the update check time: {e}");
            }
            let mut u = updates.write();
            u.busy = false;
            if let Ok(found) = &result {
                u.checked_at = Some(now);
                if matches!(found, Found::Ready { now: true, .. }) {
                    // Drops the build downloaded before, if any.
                    u.ready = waiting(&dir, current);
                }
            }
            // A failed automatic check stays quiet; it is tried again later.
            if manual || result.is_ok() {
                u.message = Some(message);
            }
        });
    }

    /// The daily check, when it is due.
    pub(super) fn auto_update_app(&self) {
        let u = self.app_updates.peek().clone();
        let due = u.checked_at.is_none_or(|t| unix_now() - t >= CHECK_EVERY_SECS);
        let resting = u.tried.is_some_and(|t| t.elapsed() < RETRY_AFTER);
        if u.auto && due && !resting && !u.busy {
            self.check_app_update(false);
        }
    }

    pub(super) fn set_auto_update_app(&self, on: bool) {
        if let Err(e) = self.library.get().set_setting(AUTO, if on { "1" } else { "0" }) {
            self.notify(format!("Couldn't save the setting: {e}"));
            return;
        }
        let mut updates = self.app_updates;
        updates.write().auto = on;
        if on {
            self.auto_update_app();
        }
    }

    /// Hands the downloaded build to Android's installer.
    pub(super) fn install_app_update(&self) {
        let mut updates = self.app_updates;
        let Some(ready) = updates.peek().ready.clone() else { return };
        if !ready.path.is_file() {
            let mut u = updates.write();
            u.ready = None;
            u.message = Some("The downloaded build is gone; check again.".into());
            return;
        }
        updates.write().message = Some(format!("Installing build {}…", ready.build));
        platform::app_update::install(&ready.path);
    }
}

/// What to say on start: that an update was installed, on the first start of
/// a build newer than the one that ran before (`last`; `None` when none did).
/// A first install says nothing.
fn updated_message(last: Option<u32>, current: u32) -> Option<String> {
    (last? < current).then(|| format!("Updated to build {current}"))
}

/// The build that ran before this start, if one did. Builds from before the
/// record didn't write it; one that checked for updates (as installing one
/// from Settings takes) was such a build, so it counts as older than any.
fn last_build(library: &Library) -> Option<u32> {
    let get = |key| library.setting(key).ok().flatten();
    get(LAST_BUILD).and_then(|s| s.parse().ok()).or(get(CHECKED).map(|_| 0))
}

/// Records this build as the one that ran, and says so when it is an update.
pub(super) fn use_updated_notice(ctx: Ctx) {
    use_hook(move || {
        let Some(current) = this_build() else { return };
        let library = ctx.library.get();
        let last = last_build(&library);
        if last == Some(current) {
            return;
        }
        if let Err(e) = library.set_setting(LAST_BUILD, &current.to_string()) {
            tracing::warn!(target: "ytmdl", "saving the build that ran: {e}");
        }
        if let Some(message) = updated_message(last, current) {
            ctx.notify(message);
        }
    });
}

/// Installs that did not replace the app report back here.
pub(super) fn use_install_results(ctx: Ctx) {
    use_hook(move || {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(i32, String)>();
        platform::app_update::listen(tx);
        spawn(async move {
            while let Some((status, message)) = rx.recv().await {
                tracing::warn!(target: "ytmdl", "update install: status {status}: {message}");
                let text = install_failure(status, &message);
                let mut updates = ctx.app_updates;
                updates.write().message = Some(text.clone());
                ctx.notify(text);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_newest_newer_build() {
        let dir = std::env::temp_dir().join(format!("ytmdl-app-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["ytmdl-56.apk", "ytmdl-57.apk", "ytmdl-58.apk", "ytmdl-60.apk", "ytmdl-61.apk.part", "junk"] {
            std::fs::write(dir.join(name), b"apk").unwrap();
        }
        let found = waiting(&dir, 57).unwrap();
        assert_eq!(found, Downloaded { build: 60, path: dir.join("ytmdl-60.apk") });
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["ytmdl-60.apk"]);
        // Installed: nothing waits any more.
        assert_eq!(waiting(&dir, 60), None);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(waiting(&dir, 1), None);
    }

    #[test]
    fn reads_the_published_description() {
        let json = r#"{"build": 58, "commit": "0123456789abcdef", "size": 71000000,
            "url": "https://github.com/o/r/releases/download/build-58/ytmdl-arm64.apk", "sha256": "ab"}"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!((release.build, release.size), (58, 71_000_000));
        assert_eq!(apk_build(Path::new("/x/ytmdl-58.apk")), Some(58));
        assert_eq!(apk_build(Path::new("/x/ytmdl-58.apk.part")), None);
        assert!(MANIFEST_URL.ends_with("/releases/latest/download/ytmdl-update.json"));
    }

    #[test]
    fn says_when_an_update_was_installed() {
        assert_eq!(updated_message(Some(14), 15).as_deref(), Some("Updated to build 15"));
        // A first install, the same build again, or an older one installed by hand.
        assert_eq!(updated_message(None, 15), None);
        assert_eq!(updated_message(Some(15), 15), None);
        assert_eq!(updated_message(Some(16), 15), None);
    }

    #[test]
    fn remembers_the_build_that_ran() {
        let art = std::env::temp_dir().join(format!("ytmdl-app-update-art-{}", std::process::id()));
        let library = Library::open_in_memory(&art).unwrap();
        assert_eq!(last_build(&library), None);
        // An older build with the updater ran: it checked, but didn't record itself.
        library.set_setting(CHECKED, "1790000000").unwrap();
        assert_eq!(last_build(&library), Some(0));
        library.set_setting(LAST_BUILD, "15").unwrap();
        assert_eq!(last_build(&library), Some(15));
    }

    #[test]
    fn describes_this_build() {
        assert_eq!(short_commit("0123456789abcdef"), "0123456");
        assert_eq!(short_commit("0123456789abcdef-dirty"), "0123456, modified");
        assert_eq!(short_commit("abc"), "abc");
        assert!(version_text().starts_with(env!("CARGO_PKG_VERSION")));
    }
}
