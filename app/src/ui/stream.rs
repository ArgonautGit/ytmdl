//! Playing songs from YouTube Music without downloading them into the
//! library. A song tapped in a list is fetched into the cache (see
//! crates/library/src/cache.rs) and played, and the songs after it in the list
//! are fetched as it plays, a couple ahead of the one playing.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dioxus::prelude::*;
use ytmdl_core::{CancelToken, DownloadOptions, Entry};
use ytmdl_library::{Library, Track};

use super::Ctx;
use crate::platform;

/// How yt-dlp names the files in the cache folder.
const TEMPLATE: &str = "%(id)s.%(ext)s";
/// Songs of the list kept ready after the one playing.
const AHEAD: usize = 2;
/// Fetches that may fail in a row before the rest of the list is dropped.
const FAILURES: u32 = 3;
/// How long a song added to the queue may take to show up in it.
const SETTLE: Duration = Duration::from_secs(10);
/// The cache's size limit (MB) in the settings.
const LIMIT: &str = "cache_mb";
pub(super) const DEFAULT_LIMIT_MB: u64 = 100;
/// The limits Settings offers.
pub(super) const LIMITS_MB: [u64; 5] = [100, 250, 500, 1000, 2000];

#[derive(Clone, Copy)]
pub(super) struct Stream {
    dir: CopyValue<PathBuf>,
    pub(super) limit_mb: Signal<u64>,
    /// The song being fetched to start playing.
    pub(super) starting: Signal<Option<Entry>>,
    /// The rest of the list being played, not queued yet.
    pub(super) pending: Signal<VecDeque<Entry>>,
    /// A song of `pending` is being fetched.
    busy: Signal<bool>,
    failures: Signal<u32>,
    /// The last queue entry the list added, and when. Once it has left the
    /// queue (another queue replaced it), the rest of the list is dropped.
    tail: Signal<Option<(String, Instant)>>,
    /// Bumped by each list played, so fetches for an older one are dropped.
    generation: Signal<u64>,
    cancel: CopyValue<CancelToken>,
    /// Bumped when the cache changes.
    pub(super) revision: Signal<u64>,
}

impl Stream {
    /// Must be called from a component (creates signals).
    pub(super) fn new(dir: PathBuf, library: &Library) -> Self {
        let limit_mb = library.setting(LIMIT).ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_LIMIT_MB);
        Stream {
            dir: CopyValue::new(dir),
            limit_mb: Signal::new(limit_mb),
            starting: Signal::new(None),
            pending: Signal::new(VecDeque::new()),
            busy: Signal::new(false),
            failures: Signal::new(0),
            tail: Signal::new(None),
            generation: Signal::new(0),
            cancel: CopyValue::new(CancelToken::new()),
            revision: Signal::new(0),
        }
    }

    fn changed(&self) {
        let mut revision = self.revision;
        *revision.write() += 1;
    }
}

impl Ctx {
    /// Plays `entries` from `start`, fetching the songs the library doesn't
    /// have into the cache as they come up.
    pub(super) fn play_entries(&self, entries: Vec<Entry>, start: usize) {
        let Some(first) = entries.get(start).cloned() else { return };
        let ctx = *self;
        let generation = self.stop_stream();
        let s = self.stream;
        let (mut starting, mut pending) = (s.starting, s.pending);
        pending.set(entries.into_iter().skip(start + 1).collect());
        starting.set(Some(first.clone()));
        let cancel = s.cancel.read().clone();
        // Something else played meanwhile wins.
        let queue = self.player.snapshot_peek().ids;
        // Not tied to the page it was started from.
        dioxus::core::spawn_forever(async move {
            let result = ctx.ready(first.clone(), false, cancel).await;
            if *s.generation.peek() != generation {
                return;
            }
            starting.set(None);
            if ctx.player.snapshot_peek().ids != queue {
                pending.write().clear();
                return;
            }
            match result {
                Ok(track) => {
                    let keys = ctx.player.play_keyed(vec![track], 0);
                    let mut tail = s.tail;
                    tail.set(keys.last().map(|k| (k.clone(), Instant::now())));
                }
                Err(e) => {
                    pending.write().clear();
                    ctx.notify(format!("Couldn't play “{}”: {e}", first.title));
                }
            }
        });
    }

    /// Stops fetching the list being played (and the song about to start);
    /// returns the new generation.
    pub(super) fn stop_stream(&self) -> u64 {
        let s = self.stream;
        s.cancel.read().cancel();
        let mut cancel = s.cancel;
        cancel.set(CancelToken::new());
        let (mut generation, mut starting, mut pending, mut busy, mut failures, mut tail) =
            (s.generation, s.starting, s.pending, s.busy, s.failures, s.tail);
        let next = *generation.peek() + 1;
        generation.set(next);
        starting.set(None);
        pending.write().clear();
        busy.set(false);
        failures.set(0);
        tail.set(None);
        next
    }

    /// Fetches the next song of the list and queues it.
    fn fetch_next(&self) {
        let ctx = *self;
        let s = self.stream;
        let (mut pending, mut busy) = (s.pending, s.busy);
        let Some(entry) = pending.write().pop_front() else { return };
        busy.set(true);
        let generation = *s.generation.peek();
        let cancel = s.cancel.read().clone();
        dioxus::core::spawn_forever(async move {
            let result = ctx.ready(entry.clone(), true, cancel).await;
            if *s.generation.peek() != generation {
                return;
            }
            let (mut failures, mut tail) = (s.failures, s.tail);
            // Another queue replaced the list's meanwhile.
            let gone = tail.peek().as_ref().is_some_and(|(key, added)| {
                added.elapsed() >= SETTLE && !ctx.player.snapshot_peek().ids.contains(key)
            });
            if gone {
                ctx.stop_stream();
                return;
            }
            match result {
                Ok(track) => {
                    failures.set(0);
                    let keys = ctx.player.add_to_queue(&[track]);
                    tail.set(keys.last().map(|k| (k.clone(), Instant::now())));
                }
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "fetching {}: {e}", entry.url);
                    let failed = *failures.peek() + 1;
                    failures.set(failed);
                    if failed >= FAILURES {
                        pending.write().clear();
                        ctx.notify(format!("Stopped getting the next songs: {e}"));
                    } else {
                        ctx.notify(format!("Couldn't get “{}”: {e}", entry.title));
                    }
                }
            }
            busy.set(false);
        });
    }

    /// `entry` ready to play: the library's song, the cached one, or else
    /// fetched into the cache (and the ReplayGain measured if `measure`,
    /// which costs a moment).
    async fn ready(&self, entry: Entry, measure: bool, cancel: CancelToken) -> Result<Track, String> {
        let library = self.library.get();
        let id = entry.id.clone();
        let found = crate::blocking(move || -> ytmdl_library::Result<Option<Track>> {
            if let Some(t) = library.track_by_video(&id)? {
                return Ok(Some(t));
            }
            let cached = library.cached(&id)?;
            if let Some(t) = &cached {
                library.touch_cached(t.id)?;
            }
            Ok(cached)
        })
        .await
        .map_err(|e| format!("{e:#}"))?
        .map_err(|e| e.to_string())?;
        if let Some(track) = found {
            return Ok(track);
        }
        let Some(svc) = self.services() else { return Err("the downloader is still starting".into()) };
        let mut opts = DownloadOptions::new(self.stream.dir.read().clone());
        opts.template = TEMPLATE.into();
        opts.track_number = entry.track_number;
        opts.lyrics = false;
        opts.measure_loudness = measure && *self.normalize.peek();
        let done = svc.dl.download(&entry.url, &opts, |_| {}, cancel).await.map_err(|e| e.to_string())?;
        let library = self.library.get();
        let track = crate::blocking(move || library.add_cached(&done))
            .await
            .map_err(|e| format!("{e:#}"))?
            .map_err(|e| e.to_string())?;
        self.stream.changed();
        self.trim_cache(None);
        Ok(track)
    }

    /// Deletes the least recently played cached songs over the size limit
    /// (or `limit` bytes), keeping the queue's.
    fn trim_cache(&self, limit: Option<u64>) {
        let ctx = *self;
        let limit = limit.unwrap_or(*self.stream.limit_mb.peek() * 1_000_000);
        let keep = self.player.queued_ids();
        let library = self.library.get();
        dioxus::core::spawn_forever(async move {
            match crate::blocking(move || library.trim_cache(limit, &keep)).await {
                Ok(Ok(0)) => {}
                Ok(Ok(_)) => ctx.stream.changed(),
                Ok(Err(e)) => tracing::warn!(target: "ytmdl", "trimming the cache: {e}"),
                Err(e) => tracing::warn!(target: "ytmdl", "trimming the cache: {e:#}"),
            }
        });
    }

    pub(super) fn set_cache_limit(&self, mb: u64) {
        if let Err(e) = self.library.get().set_setting(LIMIT, &mb.to_string()) {
            self.notify(format!("Couldn't save the setting: {e}"));
            return;
        }
        let mut limit = self.stream.limit_mb;
        limit.set(mb);
        self.trim_cache(None);
    }

    /// Empties the cache, but for the songs in the queue.
    pub(super) fn clear_cache(&self) {
        self.trim_cache(Some(0));
    }

    /// Adds a cached song to the library (its file is copied to the music folder).
    pub(super) fn keep_cached(&self, track: Track) {
        let ctx = *self;
        let Some(svc) = self.services() else {
            self.notify("The downloader is still starting".into());
            return;
        };
        let dir = svc.current_output_dir();
        let library = self.library.get();
        dioxus::core::spawn_forever(async move {
            match crate::blocking(move || library.keep_cached(track.id, &dir)).await {
                Ok(Ok(t)) => {
                    platform::media_scan(&t.path);
                    ctx.library.changed();
                    ctx.notify(format!("Added “{}” to your library", t.title));
                }
                Ok(Err(e)) => ctx.notify(format!("Couldn't add it to your library: {e}")),
                Err(e) => ctx.notify(format!("Couldn't add it to your library: {e:#}")),
            }
        });
    }
}

/// Fetches the next songs of the list being played as the player gets near
/// them, and first clears the cache folder of files an earlier run left.
pub(super) fn use_stream(ctx: Ctx) {
    use_hook(move || {
        let library = ctx.library.get();
        let dir = ctx.stream.dir.read().clone();
        spawn(async move {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(Ok(n)) = crate::blocking(move || library.sweep_cache(&dir)).await
                && n > 0
            {
                tracing::info!(target: "ytmdl", "removed {n} unfinished files from the cache");
            }
        });
    });
    use_effect(move || {
        let s = ctx.stream;
        let snapshot = ctx.player.snapshot();
        if (s.busy)() || s.pending.read().is_empty() {
            return;
        }
        let Some((tail, added)) = s.tail.read().clone() else { return };
        match snapshot.ids.iter().position(|k| *k == tail) {
            Some(at) if at < snapshot.index + AHEAD => ctx.fetch_next(),
            Some(_) => {}
            // Not in the queue yet, or no longer.
            None if added.elapsed() < SETTLE => {}
            None => {
                ctx.stop_stream();
            }
        }
    });
}
