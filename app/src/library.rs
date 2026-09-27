//! The music library as the UI sees it: the database plus a revision number that
//! screens subscribe to, bumped after every change.

use std::path::PathBuf;

use dioxus::prelude::*;
use ytmdl_library::Library;

#[derive(Clone, Copy)]
pub struct LibraryHandle {
    library: CopyValue<Library>,
    revision: Signal<u64>,
}

impl LibraryHandle {
    /// Must be called from a component (creates signals).
    pub fn new(library: Library) -> Self {
        LibraryHandle { library: CopyValue::new(library), revision: Signal::new(0) }
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
}
