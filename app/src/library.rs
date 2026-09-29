//! The music library as the UI sees it: the database plus revision numbers that
//! screens subscribe to, bumped after every change.

use std::path::PathBuf;

use dioxus::prelude::*;
use ytmdl_core::loudness;
use ytmdl_library::Library;

#[derive(Clone, Copy)]
pub struct LibraryHandle {
    library: CopyValue<Library>,
    revision: Signal<u64>,
    /// Bumped when listens come in from the player's log.
    listens: Signal<u64>,
    /// The player's listening log (see crates/library/src/listens.rs).
    listen_log: CopyValue<PathBuf>,
    importing: CopyValue<bool>,
    measuring: CopyValue<bool>,
}

impl LibraryHandle {
    /// Must be called from a component (creates signals).
    pub fn new(library: Library, listen_log: PathBuf) -> Self {
        LibraryHandle {
            library: CopyValue::new(library),
            revision: Signal::new(0),
            listens: Signal::new(0),
            listen_log: CopyValue::new(listen_log),
            importing: CopyValue::new(false),
            measuring: CopyValue::new(false),
        }
    }

    pub fn get(&self) -> Library {
        self.library.read().clone()
    }

    /// Reads the revision, so the calling memo or component reruns on changes.
    pub fn subscribe(&self) {
        let _ = (self.revision)();
    }

    pub fn changed(&self) {
        let mut revision = self.revision;
        *revision.write() += 1;
    }

    /// Like [`LibraryHandle::subscribe`], for new listens.
    pub fn subscribe_listens(&self) {
        let _ = (self.listens)();
    }

    /// Indexes new files and forgets deleted ones, off the UI thread.
    pub async fn scan(self, dirs: Vec<PathBuf>) {
        let library = self.get();
        match crate::blocking(move || library.scan(&dirs)).await {
            Ok(Ok(report)) if report.changed() => {
                tracing::info!(
                    target: "ytmdl",
                    "library scan: {} added, {} updated, {} removed",
                    report.added,
                    report.updated,
                    report.removed
                );
                self.changed();
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(target: "ytmdl", "library scan: {e}"),
            Err(e) => tracing::warn!(target: "ytmdl", "library scan: {e:#}"),
        }
    }

    /// Measures the loudness of the tracks that have no ReplayGain yet (songs
    /// downloaded before ytmdl measured it, or found by a scan) and tags their
    /// files with it, one at a time off the UI thread. About a second a song
    /// on a phone; what is done stays done if the app stops halfway.
    pub async fn measure_loudness(self) {
        let mut measuring = self.measuring;
        if *measuring.peek() {
            return;
        }
        measuring.set(true);
        let library = self.get();
        let result = crate::blocking(move || -> ytmdl_library::Result<usize> {
            let mut measured = 0;
            for track in library.unmeasured()? {
                let path = &track.path;
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                if !loudness::can_measure(&ext) {
                    library.set_gain(track.id, None, false)?;
                    continue;
                }
                match loudness::measure(path) {
                    Ok(gain) => {
                        let retagged = loudness::write(path, gain)
                            .inspect_err(|e| tracing::warn!(target: "ytmdl", "tagging the gain: {e}"))
                            .is_ok();
                        library.set_gain(track.id, Some(gain), retagged)?;
                        measured += 1;
                    }
                    // Not readable now (no storage access, say): try again next time.
                    Err(ytmdl_core::Error::Io(e)) => {
                        tracing::debug!(target: "ytmdl", "not measuring {}: {e}", path.display());
                    }
                    Err(e) => {
                        tracing::warn!(target: "ytmdl", "{e}");
                        library.set_gain(track.id, None, false)?;
                    }
                }
            }
            Ok(measured)
        })
        .await;
        measuring.set(false);
        match result {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => {
                tracing::info!(target: "ytmdl", "measured the loudness of {n} songs");
                self.changed();
            }
            Ok(Err(e)) => tracing::warn!(target: "ytmdl", "measuring loudness: {e}"),
            Err(e) => tracing::warn!(target: "ytmdl", "measuring loudness: {e:#}"),
        }
    }

    /// Takes in what the player logged since the last time, off the UI thread.
    pub async fn import_listens(self) {
        let mut importing = self.importing;
        if *importing.peek() {
            return;
        }
        importing.set(true);
        let library = self.get();
        let log = self.listen_log.read().clone();
        let result = crate::blocking(move || library.import_listens(&log)).await;
        importing.set(false);
        match result {
            Ok(Ok(0)) => {}
            Ok(Ok(_)) => {
                let mut listens = self.listens;
                *listens.write() += 1;
            }
            Ok(Err(e)) => tracing::warn!(target: "ytmdl", "importing listens: {e}"),
            Err(e) => tracing::warn!(target: "ytmdl", "importing listens: {e:#}"),
        }
    }
}
