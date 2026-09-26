//! Where the runtime pieces live and the few OS services the UI needs.

use std::path::PathBuf;

use ytmdl_core::RuntimeConfig;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::*;

/// Result of [`prepare`]: everything needed to start the runtime.
pub struct Prepared {
    pub config: RuntimeConfig,
    /// Shared music folder (needs [`has_storage_access`]).
    pub output_dir: PathBuf,
    /// App-private folder used while shared storage is not granted.
    pub fallback_output_dir: PathBuf,
}

/// Downloads run on the Python lanes; two keeps a phone responsive.
pub const DOWNLOAD_WORKERS: usize = 2;
