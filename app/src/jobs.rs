//! Download queue shared by the UI and the autotest hook.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dioxus::prelude::*;
use tokio::sync::mpsc;
use ytmdl_core::{CancelToken, DownloadOptions, Downloaded, Downloader, Entry, Error, Progress, TrackMeta};

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
    }
}

/// Handle to the job list. Progress arrives on Python worker threads and is
/// funnelled back to the UI task through a channel.
#[derive(Clone, Copy)]
pub struct Queue {
    jobs: Signal<Vec<Job>>,
    tx: Signal<mpsc::UnboundedSender<(u64, Progress)>>,
    next_id: Signal<Arc<AtomicU64>>,
}

impl Queue {
    /// Must be called from a component (creates signals and a task).
    pub fn new() -> Self {
        let jobs = Signal::new(Vec::<Job>::new());
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
        Queue { jobs, tx: Signal::new(tx), next_id: Signal::new(Arc::new(AtomicU64::new(1))) }
    }

    pub fn jobs(&self) -> Signal<Vec<Job>> {
        self.jobs
    }

    /// Queues one track and returns once it finished (or failed).
    pub async fn run(&self, svc: Services, entry: Entry) -> Result<Downloaded, Error> {
        let id = self.next_id.read().fetch_add(1, Ordering::Relaxed);
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
        let queue = *self;
        dioxus::core::spawn_forever(async move {
            if let Err(e) = queue.execute(svc, id).await {
                tracing::warn!(target: "ytmdl", "retry of job {id} failed: {e}");
            }
        });
    }

    async fn execute(&self, svc: Services, id: u64) -> Result<Downloaded, Error> {
        let mut jobs = self.jobs;
        let Some((url, cancel)) = jobs.read().iter().find(|j| j.id == id).map(|j| (j.entry.url.clone(), j.cancel.clone()))
        else {
            return Err(Error::Cancelled);
        };
        let tx = self.tx.read().clone();
        let opts = DownloadOptions::new(svc.current_output_dir());
        let token = cancel.clone();
        let on_progress = move |p| {
            // A cancelled run may have been retried already; keep its last report off the new one.
            if !token.is_cancelled() {
                drop(tx.send((id, p)));
            }
        };
        let result = svc.dl.download(&url, &opts, on_progress, cancel.clone()).await;
        if let Ok(done) = &result {
            platform::media_scan(&done.path);
        }
        let state = match &result {
            Ok(done) => JobState::Done { path: done.path.clone(), tagged: done.tagged },
            Err(Error::Cancelled) => JobState::Cancelled,
            Err(e) => JobState::Failed(e.to_string()),
        };
        // Only if the job wasn't retried meanwhile (a retry swaps in a new token).
        if let Some(job) = jobs.write().iter_mut().find(|j| j.id == id && j.cancel.ptr_eq(&cancel)) {
            job.state = state;
        }
        result
    }

    pub fn cancel(&self, id: u64) {
        let mut jobs = self.jobs;
        if let Some(job) = jobs.write().iter_mut().find(|j| j.id == id) {
            Self::cancel_job(job);
        }
    }

    /// Cancels every queued and running job; returns how many there were.
    pub fn cancel_all(&self) -> usize {
        let mut jobs = self.jobs;
        let mut list = jobs.write();
        list.iter_mut().filter(|j| j.is_active()).map(Self::cancel_job).count()
    }

    /// A running job stops at its next progress report; a waiting one is shown as
    /// cancelled right away rather than when a worker frees up.
    fn cancel_job(job: &mut Job) {
        job.cancel.cancel();
        if job.state == JobState::Queued {
            job.state = JobState::Cancelled;
        }
    }

    pub fn clear_finished(&self) {
        let mut jobs = self.jobs;
        jobs.write().retain(Job::is_active);
    }
}
