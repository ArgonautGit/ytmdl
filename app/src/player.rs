//! Playback. The Android media service (Media3, in its own process) owns the
//! queue and keeps playing with the app closed; this mirrors its state for the
//! UI and sends it commands. The queue is saved in the library so it comes back
//! after a restart.
//!
//! Queue entries have keys "<track id>.<n>", unique in the queue, so a song can
//! be queued twice and the A-B loops can name entries. The loops themselves run
//! in the service, and so does the sleep timer.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use ytmdl_library::{ART_LARGE, Library, Track};

use crate::library::LibraryHandle;
use crate::platform::player as backend;

/// What the service last reported (see `publish` in YtmdlPlayer.kt).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Snapshot {
    /// Audio is coming out.
    pub playing: bool,
    /// Playing or about to (buffering): what the play button shows.
    pub play_when_ready: bool,
    pub buffering: bool,
    pub ended: bool,
    pub index: usize,
    pub position_ms: i64,
    /// Negative until known.
    pub duration_ms: i64,
    /// Media3's repeat mode: 0 off, 1 one, 2 all.
    pub repeat: i32,
    /// Entry keys of the queue.
    pub ids: Vec<String>,
    pub error: Option<String>,
    pub song_loop: Option<SongLoop>,
    pub queue_loop: Option<QueueLoop>,
    /// The sleep timer: Unix ms to pause at.
    pub sleep_at: Option<i64>,
    /// The sleep timer pauses at the end of the song.
    pub sleep_at_end: bool,
}

/// Seeks back to `a` on reaching `b` (ms) while entry `id` plays.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SongLoop {
    pub id: String,
    pub a: i64,
    pub b: i64,
}

/// Goes back to entry `first` when entry `last` ends.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct QueueLoop {
    pub first: String,
    pub last: String,
}

/// A song in the queue.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Position in the service's queue.
    pub index: usize,
    pub key: String,
    pub track: Track,
}

/// The track an entry key ("<track id>.<n>") stands for.
fn track_id(key: &str) -> Option<i64> {
    key.split('.').next()?.parse().ok()
}

impl Snapshot {
    /// Whether the play button should offer pause. Media3 keeps play-when-ready
    /// set after the queue ends or playback fails; play then starts over or retries.
    pub fn wants_play(&self) -> bool {
        self.play_when_ready && !self.ended && self.error.is_none()
    }
}

/// When the sleep timer pauses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sleep {
    /// At this Unix time (ms).
    At(i64),
    EndOfSong,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Off,
    All,
    One,
}

/// The queue as saved between runs.
#[derive(Serialize, Deserialize)]
struct Saved {
    ids: Vec<i64>,
    index: usize,
    position_ms: i64,
}

const SAVED: &str = "player";

#[derive(Clone, Copy)]
pub struct Player {
    snapshot: Signal<Snapshot>,
    /// When `snapshot` arrived, to advance its position while playing.
    received: Signal<Instant>,
    /// Tracks deleted meanwhile are left out.
    queue: Signal<Vec<Entry>>,
    /// Bumped twice a second while playing (or a sleep timer runs) and the
    /// app is visible.
    tick: Signal<u64>,
    /// Point A of a song loop being set: (entry key, ms).
    loop_start: Signal<Option<(String, i64)>>,
    /// Next `n` for entry keys; kept past the keys the service reports, which
    /// can come from an earlier run of the app.
    serial: CopyValue<u64>,
    library: LibraryHandle,
}

impl Player {
    /// Must be called from a component (creates signals and tasks).
    pub fn new(library: LibraryHandle) -> Self {
        let player = Player {
            snapshot: Signal::new(Snapshot::default()),
            received: Signal::new(Instant::now()),
            queue: Signal::new(Vec::new()),
            tick: Signal::new(0),
            loop_start: Signal::new(None),
            serial: CopyValue::new(0),
            library,
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        spawn(async move {
            let mut first = true;
            while let Some(json) = rx.recv().await {
                match serde_json::from_str::<Snapshot>(&json) {
                    Ok(s) => player.update(s, std::mem::take(&mut first)),
                    Err(e) => tracing::warn!(target: "ytmdl", "player state {json:?}: {e}"),
                }
            }
        });
        backend::connect(tx);
        spawn(player.ticker());
        player
    }

    fn update(&self, s: Snapshot, first: bool) {
        let (mut snapshot, mut received, mut queue) = (self.snapshot, self.received, self.queue);
        // A fresh service (not playing from before this start) gets the saved queue.
        if first && s.ids.is_empty() {
            self.restore();
        }
        let old = snapshot.peek().clone();
        if s.ids != old.ids {
            queue.set(self.lookup(&s.ids));
            let mut serial = self.serial;
            let used = s.ids.iter().filter_map(|k| k.split_once('.')?.1.parse::<u64>().ok()).max();
            if let Some(used) = used.filter(|&n| n >= *serial.peek()) {
                serial.set(used + 1);
            }
        }
        if !s.ids.is_empty() && (s.ids != old.ids || s.index != old.index || s.play_when_ready != old.play_when_ready) {
            self.save(&s);
        }
        received.set(Instant::now());
        snapshot.set(s);
    }

    /// JS timers pause while the app is in the background, so this does too.
    async fn ticker(self) {
        let mut ticks = document::eval(
            "while (true) { \
                await new Promise(r => setTimeout(r, 500)); \
                if (document.visibilityState === 'visible') dioxus.send(true); \
             }",
        );
        let mut tick = self.tick;
        while ticks.recv::<bool>().await.is_ok() {
            let busy = {
                let s = self.snapshot.peek();
                s.playing || s.sleep_at.is_some()
            };
            if busy {
                *tick.write() += 1;
            }
        }
    }

    fn lookup(&self, keys: &[String]) -> Vec<Entry> {
        let ids: Vec<i64> = keys.iter().filter_map(|k| track_id(k)).collect();
        let tracks = self.library.get().tracks_by_id(&ids).unwrap_or_else(|e| {
            tracing::warn!(target: "ytmdl", "player queue: {e}");
            Vec::new()
        });
        let by_id: std::collections::HashMap<i64, Track> = tracks.into_iter().map(|t| (t.id, t)).collect();
        keys.iter()
            .enumerate()
            .filter_map(|(index, key)| Some(Entry { index, key: key.clone(), track: by_id.get(&track_id(key)?)?.clone() }))
            .collect()
    }

    /// Looks the queue up again after the library changed (a song deleted, say).
    pub fn refresh(&self) {
        let mut queue = self.queue;
        let keys = self.snapshot.peek().ids.clone();
        queue.set(self.lookup(&keys));
    }

    fn save(&self, s: &Snapshot) {
        let saved = Saved {
            ids: s.ids.iter().filter_map(|k| track_id(k)).collect(),
            index: s.index,
            position_ms: s.position_ms.max(0),
        };
        let json = serde_json::to_string(&saved).expect("plain data");
        if let Err(e) = self.library.get().set_setting(SAVED, &json) {
            tracing::warn!(target: "ytmdl", "saving the queue: {e}");
        }
    }

    fn restore(&self) {
        let library = self.library.get();
        let Some(saved) = library.setting(SAVED).ok().flatten().and_then(|s| serde_json::from_str::<Saved>(&s).ok()) else {
            return;
        };
        let tracks = library.tracks_by_id(&saved.ids).unwrap_or_default();
        let current = saved.ids.get(saved.index).and_then(|id| tracks.iter().position(|t| t.id == *id));
        if tracks.is_empty() {
            return;
        }
        let (index, position) = current.map_or((0, 0), |i| (i, saved.position_ms));
        backend::set_queue(&self.items(&tracks), index, position, false);
    }

    /// Queue entries as YtmdlPlayer takes them, with new keys.
    fn items(&self, tracks: &[Track]) -> String {
        let library = self.library.get();
        let mut serial = self.serial;
        let items: Vec<_> = tracks
            .iter()
            .map(|t| {
                let art = t.art.as_deref().and_then(|k| library.art_file(&Library::art_name(k, ART_LARGE)));
                let n = *serial.peek();
                serial.set(n + 1);
                serde_json::json!({
                    "id": format!("{}.{n}", t.id),
                    "path": t.path,
                    "title": t.title,
                    "artist": t.artists.join(", "),
                    "album": t.album.clone().unwrap_or_default(),
                    "art": art.map(|p| p.display().to_string()).unwrap_or_default(),
                })
            })
            .collect();
        serde_json::Value::Array(items).to_string()
    }

    // ---- commands ----

    /// Plays `tracks` from `start`, replacing the queue.
    pub fn play(&self, tracks: Vec<Track>, start: usize) {
        if tracks.is_empty() {
            return;
        }
        let start = start.min(tracks.len() - 1);
        backend::set_queue(&self.items(&tracks), start, 0, true);
    }

    /// Plays `tracks` in random order.
    pub fn shuffle(&self, mut tracks: Vec<Track>) {
        shuffle(&mut tracks);
        self.play(tracks, 0);
    }

    /// Queues `tracks` right after the current song.
    pub fn play_next(&self, tracks: &[Track]) {
        if !tracks.is_empty() {
            backend::insert(&self.items(tracks), true);
        }
    }

    /// Queues `tracks` at the end.
    pub fn add_to_queue(&self, tracks: &[Track]) {
        if !tracks.is_empty() {
            backend::insert(&self.items(tracks), false);
        }
    }

    /// Moves the queue entry at `from` to where the one at `to` is (positions
    /// in [`Player::queue`]).
    pub fn move_entry(&self, from: usize, to: usize) {
        let queue = self.queue.peek();
        let (Some(a), Some(b)) = (queue.get(from), queue.get(to)) else { return };
        let (from, to) = (a.index, b.index);
        drop(queue);
        if from == to {
            return;
        }
        backend::move_entry(from, to);
        // Show the new order now rather than when the service reports it.
        let mut s = self.snapshot.peek().clone();
        if from >= s.ids.len() || to >= s.ids.len() {
            return;
        }
        let key = s.ids.remove(from);
        s.ids.insert(to, key);
        s.index = match s.index {
            i if i == from => to,
            i if from < i && i <= to => i - 1,
            i if to <= i && i < from => i + 1,
            i => i,
        };
        let (mut snapshot, mut queue) = (self.snapshot, self.queue);
        queue.set(self.lookup(&s.ids));
        self.save(&s);
        snapshot.set(s);
    }

    /// Takes one entry out of the queue.
    pub fn remove_entry(&self, key: &str) {
        backend::remove(key);
    }

    /// Takes every entry of a (deleted) track out of the queue.
    pub fn remove_track(&self, id: i64) {
        backend::remove(&id.to_string());
    }

    pub fn toggle(&self) {
        if self.snapshot.peek().wants_play() {
            backend::pause();
        } else {
            backend::play();
        }
    }

    pub fn next(&self) {
        backend::next();
    }

    pub fn previous(&self) {
        backend::previous();
    }

    pub fn seek(&self, secs: f64) {
        let ms = (secs * 1000.0) as i64;
        backend::seek_to(ms);
        // Show the new position now rather than when the service reports it.
        let (mut snapshot, mut received) = (self.snapshot, self.received);
        snapshot.write().position_ms = ms;
        received.set(Instant::now());
    }

    /// Plays the `index`th song of the service's queue.
    pub fn skip_to(&self, index: usize) {
        backend::skip_to(index);
    }

    /// The song A-B button: sets A at the current position, then B, then clears
    /// the loop. B before A swaps them.
    pub fn song_loop_step(&self) {
        let Some(key) = self.current_key_peek() else { return };
        let ms = (self.position_secs_peek() * 1000.0) as i64;
        let mut start = self.loop_start;
        if self.snapshot.peek().song_loop.is_some() {
            start.set(None);
            backend::set_song_loop(None);
            return;
        }
        let pending = start.peek().clone().filter(|(k, _)| *k == key);
        match pending {
            None => start.set(Some((key, ms))),
            // Too short to loop; keep waiting for B.
            Some((_, a)) if (ms - a).abs() < 500 => {}
            Some((key, a)) => {
                start.set(None);
                backend::set_song_loop(Some((&key, a.min(ms), a.max(ms))));
            }
        }
    }

    /// Loops the current song from `a_ms` to `b_ms` (a saved section), from A.
    pub fn loop_section(&self, a_ms: i64, b_ms: i64) {
        let Some(key) = self.current_key_peek() else { return };
        let mut start = self.loop_start;
        start.set(None);
        backend::set_song_loop(Some((&key, a_ms, b_ms)));
        self.seek(a_ms as f64 / 1000.0);
        let mut snapshot = self.snapshot;
        snapshot.write().song_loop = Some(SongLoop { id: key, a: a_ms, b: b_ms });
    }

    /// Ends the current song's A-B loop (or forgets its A).
    pub fn stop_song_loop(&self) {
        let (mut start, mut snapshot) = (self.loop_start, self.snapshot);
        start.set(None);
        backend::set_song_loop(None);
        snapshot.write().song_loop = None;
    }

    /// Sets the sleep timer, or turns it off.
    pub fn set_sleep(&self, sleep: Option<Sleep>) {
        let (at, end) = match sleep {
            None => (0, false),
            Some(Sleep::At(ms)) => (ms, false),
            Some(Sleep::EndOfSong) => (0, true),
        };
        backend::set_sleep(at, end);
        let mut snapshot = self.snapshot;
        let mut s = snapshot.write();
        s.sleep_at = (at > 0).then_some(at);
        s.sleep_at_end = end;
    }

    /// Loops the queue from entry `first` to entry `last`, in queue order.
    pub fn set_queue_loop(&self, first: &str, last: &str) {
        let ids = &self.snapshot.peek().ids;
        let (Some(a), Some(b)) = (ids.iter().position(|k| k == first), ids.iter().position(|k| k == last)) else {
            return;
        };
        let (first, last) = if a <= b { (first, last) } else { (last, first) };
        backend::set_queue_loop(Some((first, last)));
    }

    pub fn clear_queue_loop(&self) {
        backend::set_queue_loop(None);
    }

    /// Off, then all, then one.
    pub fn cycle_repeat(&self) {
        let mode = match self.repeat() {
            Repeat::Off => 2,
            Repeat::All => 1,
            Repeat::One => 0,
        };
        backend::set_repeat(mode);
    }

    // ---- state (reading subscribes) ----

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().clone()
    }

    pub fn queue(&self) -> Vec<Entry> {
        self.queue.read().clone()
    }

    pub fn current(&self) -> Option<Track> {
        let index = self.snapshot.read().index;
        self.queue.read().iter().find(|e| e.index == index).map(|e| e.track.clone())
    }

    /// Track id of the current song.
    pub fn current_id(&self) -> Option<i64> {
        self.current_key().as_deref().and_then(track_id)
    }

    /// Entry key of the current song.
    pub fn current_key(&self) -> Option<String> {
        let s = self.snapshot.read();
        s.ids.get(s.index).cloned()
    }

    fn current_key_peek(&self) -> Option<String> {
        let s = self.snapshot.peek();
        s.ids.get(s.index).cloned()
    }

    /// The current song's loop: A (ms), and B once set.
    pub fn song_loop(&self) -> Option<(i64, Option<i64>)> {
        let key = self.current_key()?;
        if let Some(l) = self.snapshot.read().song_loop.as_ref().filter(|l| l.id == key) {
            return Some((l.a, Some(l.b)));
        }
        self.loop_start.read().as_ref().filter(|(k, _)| *k == key).map(|(_, a)| (*a, None))
    }

    pub fn queue_loop(&self) -> Option<QueueLoop> {
        self.snapshot.read().queue_loop.clone()
    }

    pub fn sleep(&self) -> Option<Sleep> {
        let s = self.snapshot.read();
        if s.sleep_at_end { Some(Sleep::EndOfSong) } else { s.sleep_at.map(Sleep::At) }
    }

    pub fn repeat(&self) -> Repeat {
        match self.snapshot.read().repeat {
            1 => Repeat::One,
            2 => Repeat::All,
            _ => Repeat::Off,
        }
    }

    /// Duration of the current song, from the service or else the library.
    pub fn duration_secs(&self) -> f64 {
        let ms = self.snapshot.read().duration_ms;
        if ms > 0 { ms as f64 / 1000.0 } else { self.current().and_then(|t| t.duration_secs).unwrap_or(0.0) }
    }

    /// Where the current song is, advanced from the last report while playing;
    /// rerenders twice a second while playing.
    pub fn position_secs(&self) -> f64 {
        let _ = (self.tick)();
        position(&self.snapshot.read(), *self.received.read())
    }

    fn position_secs_peek(&self) -> f64 {
        position(&self.snapshot.peek(), *self.received.peek())
    }
}

fn position(s: &Snapshot, received: Instant) -> f64 {
    let mut ms = s.position_ms as f64;
    if s.playing {
        ms += received.elapsed().as_millis() as f64;
    }
    let secs = ms.max(0.0) / 1000.0;
    let duration = if s.duration_ms > 0 { s.duration_ms as f64 / 1000.0 } else { f64::INFINITY };
    secs.min(duration)
}

/// Fisher-Yates with xorshift; good enough for a play order.
fn shuffle<T>(items: &mut [T]) {
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0x2545_f491_4f6c_dd1d, |d| d.as_nanos() as u64);
    let mut x = seed | 1;
    for i in (1..items.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        items.swap(i, (x % (i as u64 + 1)) as usize);
    }
}
