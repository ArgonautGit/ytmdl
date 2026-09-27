//! Download queue shared by the UI and the autotest hook.

use std::path::PathBuf;

use dioxus::prelude::*;
use tokio::sync::mpsc;
use ytmdl_core::{CancelToken, DownloadOptions, Downloaded, Downloader, Entry, Error, Progress, TrackMeta};
use ytmdl_library::{SavedDownload, SavedState};

use crate::library::LibraryHandle;
use crate::platform;

/// Started runtime plus where downloads go.
#[derive(Clone)]
pub struct Services {
    pub dl: Downloader,
    pub output_dir: PathBuf,
    pub fallback_output_dir: PathBuf,
}

impl Services {
    pub fn current_output_dir(&self) -> PathBuf {
        if platform::has_storage_access() { self.output_dir.clone() } else { self.fallback_output_dir.clone() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Queued,
    Downloading,
    Done { path: PathBuf, tagged: bool },
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: u64,
    /// What was asked for: the search result or collection member.
    pub entry: Entry,
    pub state: JobState,
    pub progress: Option<Progress>,
    pub cancel: CancelToken,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.entry == other.entry && self.state == other.state && self.progress == other.progress
    }
}

impl Job {
    pub fn is_active(&self) -> bool {
        matches!(self.state, JobState::Queued | JobState::Downloading)
    }

    fn from_saved(saved: SavedDownload) -> Job {
        let state = match saved.state {
            SavedState::Pending => JobState::Queued,
            SavedState::Done { path, tagged } => JobState::Done { path, tagged },
            SavedState::Failed(e) => JobState::Failed(e),
            SavedState::Cancelled => JobState::Cancelled,
        };
        Job { id: saved.id as u64, entry: saved.entry, state, progress: None, cancel: CancelToken::new() }
    }
}

impl JobState {
    fn saved(&self) -> SavedState {
        match self {
            JobState::Queued | JobState::Downloading => SavedState::Pending,
            JobState::Done { path, tagged } => SavedState::Done { path: path.clone(), tagged: *tagged },
            JobState::Failed(e) => SavedState::Failed(e.clone()),
            JobState::Cancelled => SavedState::Cancelled,
        }
    }
}

/// A track resolved from a pasted link, as a list entry.
pub fn entry_from_track(t: TrackMeta) -> Entry {
    Entry {
        id: t.id,
        url: t.url,
        title: t.title,
        artists: t.artists,
        album: t.album,
        duration_secs: t.duration_secs,
        thumbnail: t.cover_urls.first().map(|u| ytmdl_core::art_url(u, 226)),
        kind: Some("song".into()),
        year: t.year,
        track_number: t.track_number,
    }
}

/// Handle to the job list, which is saved in the library database so downloads
/// survive restarts. Progress arrives on Python worker threads and is funnelled
/// back to the UI task through a channel.
#[derive(Clone, Copy)]
pub struct Queue {
    jobs: Signal<Vec<Job>>,
    tx: Signal<mpsc::UnboundedSender<(u64, Progress)>>,
    library: LibraryHandle,
    /// Ids for jobs the database could not record (not saved; far above row ids).
    unsaved_ids: Signal<u64>,
}

impl Queue {
    /// Must be called from a component (creates signals and a task). Loads the
    /// saved jobs; pending ones wait for [`Queue::resume`].
    pub fn new(library: LibraryHandle) -> Self {
        let saved = library.get().download_jobs().unwrap_or_else(|e| {
            tracing::warn!(target: "ytmdl", "loading saved downloads: {e}");
            Vec::new()
        });
        let jobs = Signal::new(saved.into_iter().map(Job::from_saved).collect::<Vec<_>>());
        let (tx, mut rx) = mpsc::unbounded_channel::<(u64, Progress)>();
        let mut writer = jobs;
        spawn(async move {
            while let Some((id, progress)) = rx.recv().await {
                if let Some(job) = writer.write().iter_mut().find(|j| j.id == id) {
                    if job.state == JobState::Queued {
                        job.state = JobState::Downloading;
                    }
                    job.progress = Some(progress);
                }
            }
        });
        Queue { jobs, tx: Signal::new(tx), library, unsaved_ids: Signal::new(1 << 48) }
    }

    pub fn jobs(&self) -> Signal<Vec<Job>> {
        self.jobs
    }

    /// Starts the jobs that were pending when the app last stopped; returns how many.
    pub fn resume(&self, svc: Services) -> usize {
        let pending: Vec<u64> =
            self.jobs.peek().iter().filter(|j| j.state == JobState::Queued).map(|j| j.id).collect();
        for &id in &pending {
            self.spawn_execute(svc.clone(), id);
        }
        pending.len()
    }

    /// Queues one track and returns once it finished (or failed).
    pub async fn run(&self, svc: Services, entry: Entry) -> Result<Downloaded, Error> {
        let id = match self.library.get().add_download_job(&entry) {
            Ok(id) => id as u64,
            Err(e) => {
                tracing::warn!(target: "ytmdl", "saving download: {e}");
                let mut next = self.unsaved_ids;
                let id = *next.peek();
                next.set(id + 1);
                id
            }
        };
        let mut jobs = self.jobs;
        jobs.write().push(Job { id, entry, state: JobState::Queued, progress: None, cancel: CancelToken::new() });
        self.execute(svc, id).await
    }

    /// Fire-and-forget [`Queue::run`] for UI buttons. The task belongs to the root
    /// scope: with plain `spawn` it would die with the row that was tapped, leaving
    /// the job "downloading" forever and the file unscanned.
    pub fn start(&self, svc: Services, entry: Entry) {
        let queue = *self;
        dioxus::core::spawn_forever(async move {
            let title = entry.title.clone();
            if let Err(e) = queue.run(svc, entry).await {
                tracing::warn!(target: "ytmdl", "download of {title:?} failed: {e}");
            }
        });
    }

    /// Runs a failed or cancelled job again, in place.
    pub fn retry(&self, svc: Services, id: u64) {
        let mut jobs = self.jobs;
        {
            let mut list = jobs.write();
            let Some(job) = list.iter_mut().find(|j| j.id == id && !j.is_active()) else { return };
            job.state = JobState::Queued;
            job.progress = None;
            job.cancel = CancelToken::new();
        }
        self.save(id, &JobState::Queued);
        self.spawn_execute(svc, id);
    }

    fn spawn_execute(&self, svc: Services, id: u64) {
        let queue = *self;
        dioxus::core::spawn_forever(async move {
            if let Err(e) = queue.execute(svc, id).await {
                tracing::warn!(target: "ytmdl", "download job {id} failed: {e}");
            }
        });
    }

    async fn execute(&self, svc: Services, id: u64) -> Result<Downloaded, Error> {
        let mut jobs = self.jobs;
        let Some((entry, cancel)) = jobs.read().iter().find(|j| j.id == id).map(|j| (j.entry.clone(), j.cancel.clone()))
        else {
            return Err(Error::Cancelled);
        };
        let tx = self.tx.read().clone();
        let mut opts = DownloadOptions::new(svc.current_output_dir());
        opts.track_number = entry.track_number;
        opts.lyrics = crate::ui::lyrics_lookup_enabled(&self.library.get());
        let token = cancel.clone();
        let on_progress = move |p| {
            // A cancelled run may have been retried already; keep its last report off the new one.
            if !token.is_cancelled() {
                drop(tx.send((id, p)));
            }
        };
        let result = svc.dl.download(&entry.url, &opts, on_progress, cancel.clone()).await;
        if let Ok(done) = &result {
            platform::media_scan(&done.path);
            self.add_to_library(done.clone()).await;
        }
        let state = match &result {
            Ok(done) => JobState::Done { path: done.path.clone(), tagged: done.tagged },
            Err(Error::Cancelled) => JobState::Cancelled,
            Err(e) => JobState::Failed(e.to_string()),
        };
        // Only if the job wasn't retried meanwhile (a retry swaps in a new token).
        let current = jobs.write().iter_mut().find(|j| j.id == id && j.cancel.ptr_eq(&cancel)).map(|job| {
            job.state = state.clone();
        });
        if current.is_some() {
            self.save(id, &state);
        }
        result
    }

    /// Indexes a finished download (reads its cover art, so off the UI thread).
    async fn add_to_library(&self, done: Downloaded) {
        let library = self.library.get();
        match crate::blocking(move || library.add_download(&done)).await {
            Ok(Ok(_)) => self.library.changed(),
            Ok(Err(e)) => tracing::warn!(target: "ytmdl", "adding to the library: {e}"),
            Err(e) => tracing::warn!(target: "ytmdl", "adding to the library: {e:#}"),
        }
    }

    fn save(&self, id: u64, state: &JobState) {
        if let Err(e) = self.library.get().set_download_state(id as i64, &state.saved()) {
            tracing::warn!(target: "ytmdl", "saving download {id}: {e}");
        }
    }

    pub fn cancel(&self, id: u64) {
        let mut jobs = self.jobs;
        let was_queued = jobs.write().iter_mut().find(|j| j.id == id).is_some_and(Self::cancel_job);
        if was_queued {
            self.save(id, &JobState::Cancelled);
        }
    }

    /// Cancels every queued and running job; returns how many there were.
    pub fn cancel_all(&self) -> usize {
        let mut jobs = self.jobs;
        let mut queued = Vec::new();
        let count = jobs
            .write()
            .iter_mut()
            .filter(|j| j.is_active())
            .inspect(|j| {
                if j.state == JobState::Queued {
                    queued.push(j.id);
                }
            })
            .map(Self::cancel_job)
            .count();
        for id in queued {
            self.save(id, &JobState::Cancelled);
        }
        count
    }

    /// A running job stops at its next progress report; a waiting one is shown as
    /// cancelled right away rather than when a worker frees up (it may not have
    /// been started at all). Returns whether it was waiting.
    fn cancel_job(job: &mut Job) -> bool {
        job.cancel.cancel();
        let queued = job.state == JobState::Queued;
        if queued {
            job.state = JobState::Cancelled;
        }
        queued
    }

    pub fn clear_finished(&self) {
        let mut jobs = self.jobs;
        jobs.write().retain(Job::is_active);
        if let Err(e) = self.library.get().remove_finished_jobs() {
            tracing::warn!(target: "ytmdl", "clearing finished downloads: {e}");
        }
    }
}
