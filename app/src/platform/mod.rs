//! Where the runtime pieces live and the few OS services the UI needs.

use std::path::PathBuf;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::*;

/// Where things live; cheap to look up, unlike [`prepare`].
#[derive(Clone, Debug)]
pub struct Dirs {
    /// App-private data: the library database, cover art, yt-dlp updates.
    pub data: PathBuf,
    pub cache: PathBuf,
    /// Shared music folder (needs [`has_storage_access`]).
    pub music: PathBuf,
    /// App-private folder used while shared storage is not granted.
    pub fallback_music: PathBuf,
}

impl Dirs {
    /// Folders downloads can be in: the shared one only when it is readable.
    pub fn music_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.fallback_music.clone()];
        if has_storage_access() && self.music != self.fallback_music {
            dirs.insert(0, self.music.clone());
        }
        dirs
    }
}

/// Downloads run on the Python lanes; two keeps a phone responsive.
pub const DOWNLOAD_WORKERS: usize = 2;
