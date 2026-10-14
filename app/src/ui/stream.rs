//! Playing songs from YouTube Music without downloading them into the
//! library. A song tapped in a list is fetched into the cache (see
//! crates/library/src/cache.rs) and played, and the songs after it in the list
//! are fetched as it plays, a couple ahead of the one playing.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dioxus::prelude::*;
use ytmdl_core::{CancelToken, DownloadOptions, Entry, is_unavailable_message};
use ytmdl_library::{Library, Track};

use super::Ctx;
use crate::platform;
use crate::player::Entry as QueueEntry;

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
    /// Songs of the list dragged up among the queue's, being fetched to go
    /// there.
    pub(super) placing: Signal<Vec<Placing>>,
    /// A song of `pending` is being fetched.
    busy: Signal<bool>,
    failures: Signal<u32>,
    /// Songs YouTube has no video for (not on the failure count; skipped when
    /// they come up again).
    unavailable: CopyValue<HashSet<String>>,
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
            placing: Signal::new(Vec::new()),
            busy: Signal::new(false),
            failures: Signal::new(0),
            unavailable: CopyValue::new(HashSet::new()),
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

/// A song of the list on its way into the queue.
#[derive(Clone, PartialEq)]
pub(super) struct Placing {
    pub(super) entry: Entry,
    /// The queue entry it goes after; first if `None`.
    after: Option<String>,
    /// Its queue entry, once added (it moves into place when the queue shows it).
    key: Option<String>,
}

/// A row of the queue on Now Playing: the queue's songs, then the rest of the
/// list being played.
#[derive(Clone, PartialEq)]
pub(super) enum Row {
    Queued(QueueEntry),
    /// Fetched to go here.
    Placing(Entry),
    /// The `n`th of the list's rest.
    Pending(usize, Entry),
}

/// The rows of `queue`, with the songs being placed in it and the list's rest.
pub(super) fn rows(queue: Vec<QueueEntry>, placing: &[Placing], pending: &VecDeque<Entry>) -> Vec<Row> {
    let queued = |key: &String| queue.iter().any(|e| e.key == *key);
    let shown: Vec<&Placing> = placing.iter().filter(|p| !p.key.as_ref().is_some_and(queued)).collect();
    let placed = |after: Option<&String>| -> Vec<Row> {
        shown.iter().filter(|p| p.after.as_ref() == after).map(|p| Row::Placing(p.entry.clone())).collect()
    };
    let mut rows = placed(None);
    for e in &queue {
        rows.push(Row::Queued(e.clone()));
        rows.extend(placed(Some(&e.key)));
    }
    // Their place left the queue meanwhile.
    rows.extend(shown.iter().filter(|p| p.after.as_ref().is_some_and(|a| !queued(a))).map(|p| Row::Placing(p.entry.clone())));
    rows.extend(pending.iter().cloned().enumerate().map(|(i, e)| Row::Pending(i, e)));
    rows
}

/// What the list knows of a song of the queue, to fetch it again later.
fn entry_of(t: &Track) -> Entry {
    Entry {
        id: t.video_id.clone(),
        url: t.url.clone(),
        title: t.title.clone(),
        artists: t.artists.clone(),
        album: t.album.clone(),
        duration_secs: t.duration_secs,
        thumbnail: t.art.as_deref().map(|k| format!("/art/{}", Library::art_name(k, ytmdl_library::ART_SMALL))),
        kind: Some("song".into()),
        year: t.year,
        track_number: t.track_number,
    }
}

impl Ctx {
    /// The rows of the queue on Now Playing.
    pub(super) fn queue_rows(&self) -> Vec<Row> {
        rows(self.player.queue(), &self.stream.placing.read(), &self.stream.pending.read())
    }

    /// Moves the row at `from` to `to` (positions in [`Ctx::queue_rows`]). A
    /// song dropped among the queue's goes there, fetched first if it is
    /// still to come; one dropped among the songs to come goes back to them.
    pub(super) fn move_row(&self, from: usize, to: usize) {
        let s = self.stream;
        let mut rows = rows(self.player.queue_peek(), &s.placing.peek(), &s.pending.peek());
        if from >= rows.len() || to >= rows.len() || from == to {
            return;
        }
        let row = rows.remove(from);
        // It lands after the row it passed going down, before it going up.
        let pending = if to > from { matches!(rows[to - 1], Row::Pending(..)) } else { matches!(rows[to], Row::Pending(..)) };
        let before = &rows[..to];
        let queued_before = before.iter().filter(|r| matches!(r, Row::Queued(_))).count();
        let pending_before = before.iter().filter(|r| matches!(r, Row::Pending(..))).count();
        let player = self.player;
        match row {
            Row::Placing(_) => {}
            Row::Queued(e) => {
                let at = player.queue_peek().iter().position(|q| q.key == e.key);
                let Some(at) = at else { return };
                let playing = player.current_key_peek().as_deref() == Some(e.key.as_str());
                if !pending || playing || e.track.video_id.is_empty() {
                    player.move_entry(at, queued_before.min(player.queue_peek().len() - 1));
                    return;
                }
                let mut tail = s.tail;
                if tail.peek().as_ref().is_some_and(|(k, _)| *k == e.key) {
                    let last = player.queue_peek().into_iter().rev().find(|q| q.key != e.key).map(|q| q.key);
                    tail.set(last.map(|k| (k, Instant::now())));
                }
                let mut list = s.pending;
                list.write().insert(pending_before, entry_of(&e.track));
                player.remove_entry(&e.key);
            }
            Row::Pending(i, entry) => {
                let mut list = s.pending;
                list.write().remove(i);
                if pending {
                    list.write().insert(pending_before, entry);
                    return;
                }
                let after = before.iter().rev().find_map(|r| match r {
                    Row::Queued(q) => Some(q.key.clone()),
                    _ => None,
                });
                self.place(entry, after);
            }
        }
    }

    /// Fetches a song of the list to go in the queue after `after`.
    fn place(&self, entry: Entry, after: Option<String>) {
        let ctx = *self;
        let s = self.stream;
        let mut placing = s.placing;
        placing.write().push(Placing { entry: entry.clone(), after, key: None });
        let generation = *s.generation.peek();
        let cancel = s.cancel.read().clone();
        dioxus::core::spawn_forever(async move {
            let result = ctx.ready(entry.clone(), true, cancel).await;
            if *s.generation.peek() != generation {
                return;
            }
            let at = placing.peek().iter().position(|p| p.entry.id == entry.id && p.key.is_none());
            let Some(at) = at else { return };
            match result {
                Ok(track) => {
                    let key = ctx.player.add_from_list(&[track]).pop();
                    placing.write()[at].key = key;
                }
                Err(e) => {
                    placing.write().remove(at);
                    ctx.notify(format!("Couldn't get “{}”: {e}", entry.title));
                }
            }
        });
    }

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
                Err(e) if is_unavailable_message(&e) && !pending.peek().is_empty() => {
                    tracing::info!(target: "ytmdl", "skipping {}: {e}", first.url);
                    let mut unavailable = ctx.stream.unavailable;
                    unavailable.write().insert(first.id.clone());
                    ctx.notify(format!("“{}” isn't available on YouTube, skipped", first.title));
                    let rest = std::mem::take(&mut *pending.write());
                    ctx.play_entries(rest.into(), 0);
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
        let mut placing = s.placing;
        placing.write().clear();
        busy.set(false);
        failures.set(0);
        tail.set(None);
        next
    }

    /// Takes the `i`th song of the list's rest out of it.
    pub(super) fn drop_pending(&self, i: usize) {
        let mut pending = self.stream.pending;
        pending.write().remove(i);
    }

    /// Fetches the next song of the list and queues it.
    fn fetch_next(&self) {
        let ctx = *self;
        let s = self.stream;
        let (mut pending, mut busy) = (s.pending, s.busy);
        let known = s.unavailable.read().clone();
        let entry = loop {
            let Some(next) = pending.write().pop_front() else { return };
            if !known.contains(&next.id) {
                break next;
            }
        };
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
                    let keys = ctx.player.add_from_list(&[track]);
                    tail.set(keys.last().map(|k| (k.clone(), Instant::now())));
                }
                // Nothing is wrong with the app or the network: the song is just gone.
                Err(e) if is_unavailable_message(&e) => {
                    tracing::info!(target: "ytmdl", "skipping {}: {e}", entry.url);
                    let mut unavailable = s.unavailable;
                    unavailable.write().insert(entry.id.clone());
                    ctx.notify(format!("“{}” isn't available on YouTube, skipped", entry.title));
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

    /// Adds a cached song to the library (its file is copied to the music
    /// folder), and so to `playlist` if it was wanted there.
    pub(super) fn keep_cached(&self, track: Track, playlist: Option<String>) {
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
                    ctx.notify(match playlist {
                        Some(name) => format!("Added “{}” to your library and {name}", t.title),
                        None => format!("Added “{}” to your library", t.title),
                    });
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
    // A song fetched to go among the queue's moves there once the queue shows it.
    use_effect(move || {
        let queue = ctx.player.queue();
        let mut placing = ctx.stream.placing;
        let arrived: Vec<Placing> = placing
            .peek()
            .iter()
            .filter(|p| p.key.as_ref().is_some_and(|k| queue.iter().any(|e| e.key == *k)))
            .cloned()
            .collect();
        if arrived.is_empty() {
            return;
        }
        placing.write().retain(|p| !arrived.contains(p));
        for p in arrived {
            let queue = ctx.player.queue_peek();
            let Some(at) = queue.iter().position(|e| Some(&e.key) == p.key.as_ref()) else { continue };
            let to = match &p.after {
                None => 0,
                Some(a) => match queue.iter().position(|e| e.key == *a) {
                    Some(b) if b < at => b + 1,
                    Some(b) => b,
                    None => continue,
                },
            };
            ctx.player.move_entry(at, to);
        }
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
