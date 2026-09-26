//! Desktop: same environment as the CLI (the nix devshell sets these).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ytmdl_core::RuntimeConfig;

use super::{DOWNLOAD_WORKERS, Prepared};

pub fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,ytmdl=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// Runtime paths from the environment, falling back to the values the devshell had
/// when this binary was built.
pub fn prepare() -> Result<Prepared> {
    let var = |name: &str, built: Option<&str>| {
        std::env::var_os(name).map(PathBuf::from).or_else(|| built.map(PathBuf::from))
    };
    let python_home = var("YTMDL_PYTHON_HOME", option_env!("YTMDL_PYTHON_HOME"))
        .context("set YTMDL_PYTHON_HOME (the nix devshell does)")?;
    let ytdlp_seed =
        var("YTMDL_YTDLP", option_env!("YTMDL_YTDLP")).context("set YTMDL_YTDLP (run tools/fetch-deps)")?;
    let qjs = var("YTMDL_QJS", option_env!("YTMDL_QJS")).context("set YTMDL_QJS")?;

    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
    let xdg = |name: &str, fallback: &str| {
        std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| home.join(fallback)).join("ytmdl")
    };
    let data = xdg("XDG_DATA_HOME", ".local/share");
    let cache = xdg("XDG_CACHE_HOME", ".cache");
    let output_dir = std::env::var_os("YTMDL_OUTPUT").map(PathBuf::from).unwrap_or_else(|| home.join("Music"));
    Ok(Prepared {
        config: RuntimeConfig {
            python_home,
            ytdlp_seed,
            ytdlp_dir: data.join("yt-dlp"),
            qjs,
            tmp_dir: cache.join("tmp"),
            cache_dir: cache,
            download_workers: DOWNLOAD_WORKERS,
        },
        fallback_output_dir: output_dir.clone(),
        output_dir,
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
