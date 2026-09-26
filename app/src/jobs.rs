//! Download queue shared by the UI and the autotest hook.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dioxus::prelude::*;
use tokio::sync::mpsc;
use ytmdl_core::{CancelToken, DownloadOptions, Downloaded, Downloader, Error, Progress};

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

#[derive(Clone, PartialEq)]
pub enum JobState {
    Queued,
    Downloading,
    Done { path: PathBuf, tagged: bool },
    Failed(String),
    Cancelled,
}

#[derive(Clone)]
pub struct Job {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub state: JobState,
    pub progress: Option<Progress>,
    pub cancel: CancelToken,
}

impl Job {
    pub fn is_active(&self) -> bool {
        matches!(self.state, JobState::Queued | JobState::Downloading)
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
    pub async fn run(&self, svc: Services, url: String, title: String, artist: String) -> Result<Downloaded, Error> {
        let id = self.next_id.read().fetch_add(1, Ordering::Relaxed);
        let cancel = CancelToken::new();
        let mut jobs = self.jobs;
        jobs.write().push(Job {
            id,
            title,
            artist,
            state: JobState::Queued,
            progress: None,
            cancel: cancel.clone(),
        });

        let tx = self.tx.read().clone();
        let opts = DownloadOptions::new(svc.current_output_dir());
        let result = svc.dl.download(&url, &opts, move |p| drop(tx.send((id, p))), cancel).await;
        if let Ok(done) = &result {
            platform::media_scan(&done.path);
        }
        let state = match &result {
            Ok(done) => JobState::Done { path: done.path.clone(), tagged: done.tagged },
            Err(Error::Cancelled) => JobState::Cancelled,
            Err(e) => JobState::Failed(e.to_string()),
        };
        if let Some(job) = jobs.write().iter_mut().find(|j| j.id == id) {
            job.state = state;
        }
        result
    }

    /// Fire-and-forget [`Queue::run`] for UI buttons. The task belongs to the root
    /// scope: with plain `spawn` it would die with the row that was tapped, leaving
    /// the job "downloading" forever and the file unscanned.
    pub fn start(&self, svc: Services, url: String, title: String, artist: String) {
        let queue = *self;
        dioxus::core::spawn_forever(async move {
            if let Err(e) = queue.run(svc, url, title.clone(), artist).await {
                tracing::warn!(target: "ytmdl", "download of {title:?} failed: {e}");
            }
        });
    }

    pub fn clear_finished(&self) {
        let mut jobs = self.jobs;
        jobs.write().retain(Job::is_active);
    }
}
