//! Desktop: same environment as the CLI (the nix devshell sets these).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ytmdl_core::RuntimeConfig;

use super::{DOWNLOAD_WORKERS, Dirs};

pub fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,ytmdl=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// XDG data and cache folders; downloads go to `$YTMDL_OUTPUT` or ~/Music.
pub fn dirs() -> Result<Dirs> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
    let xdg = |name: &str, fallback: &str| {
        std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| home.join(fallback)).join("ytmdl")
    };
    let music = std::env::var_os("YTMDL_OUTPUT").map(PathBuf::from).unwrap_or_else(|| home.join("Music"));
    Ok(Dirs {
        data: xdg("XDG_DATA_HOME", ".local/share"),
        cache: xdg("XDG_CACHE_HOME", ".cache"),
        fallback_music: music.clone(),
        music,
    })
}

/// Runtime paths from the environment, falling back to the values the devshell had
/// when this binary was built.
pub fn prepare(dirs: &Dirs) -> Result<RuntimeConfig> {
    let var = |name: &str, built: Option<&str>| {
        std::env::var_os(name).map(PathBuf::from).or_else(|| built.map(PathBuf::from))
    };
    let python_home = var("YTMDL_PYTHON_HOME", option_env!("YTMDL_PYTHON_HOME"))
        .context("set YTMDL_PYTHON_HOME (the nix devshell does)")?;
    let ytdlp_seed =
        var("YTMDL_YTDLP", option_env!("YTMDL_YTDLP")).context("set YTMDL_YTDLP (run tools/fetch-deps)")?;
    let qjs = var("YTMDL_QJS", option_env!("YTMDL_QJS")).context("set YTMDL_QJS")?;
    Ok(RuntimeConfig {
        python_home,
        ytdlp_seed,
        ytdlp_dir: dirs.data.join("yt-dlp"),
        qjs,
        tmp_dir: dirs.cache.join("tmp"),
        cache_dir: dirs.cache.clone(),
        download_workers: DOWNLOAD_WORKERS,
    })
}

/// URL to download and report on at startup (`autotest` builds), for end-to-end tests.
pub fn autotest_url() -> Option<String> {
    if cfg!(feature = "autotest") { std::env::var("YTMDL_AUTOTEST").ok() } else { None }
}

pub fn has_storage_access() -> bool {
    true
}

pub fn request_storage_access() {}

pub fn media_scan(_path: &Path) {}

/// No playback on desktop (the app is only built there for checks); the player
/// never reports a state, so the UI shows no player.
pub mod player {
    use tokio::sync::mpsc::UnboundedSender;

    pub fn connect(_states: UnboundedSender<String>) {}
    pub fn set_queue(_items: &str, _index: usize, _position_ms: i64, _play: bool) {}
    pub fn play() {}
    pub fn pause() {}
    pub fn next() {}
    pub fn previous() {}
    pub fn seek_to(_position_ms: i64) {}
    pub fn skip_to(_index: usize) {}
    pub fn set_repeat(_mode: i32) {}
    pub fn insert(_items: &str, _next: bool) {}
    pub fn remove(_key: &str) {}
    pub fn set_song_loop(_song: Option<(&str, i64, i64)>) {}
    pub fn set_queue_loop(_range: Option<(&str, &str)>) {}
}

/// Desktop processes aren't frozen in the background.
pub mod downloads {
    pub fn update(_title: &str, _text: &str) {}
    pub fn stop() {}
    pub fn ask_notifications() {}
}
