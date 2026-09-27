//! The music library as the UI sees it: the database plus revision numbers that
//! screens subscribe to, bumped after every change.

use std::path::PathBuf;

use dioxus::prelude::*;
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
            Ok(Ok(report)) if report.changed() => self.changed(),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(target: "ytmdl", "library scan: {e}"),
            Err(e) => tracing::warn!(target: "ytmdl", "library scan: {e:#}"),
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
