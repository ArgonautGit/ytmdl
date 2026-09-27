//! Playback. The Android media service (Media3, in its own process) owns the
//! queue and keeps playing with the app closed; this mirrors its state for the
//! UI and sends it commands. The queue is saved in the library so it comes back
//! after a restart.

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
    /// Track ids of the queue.
    pub ids: Vec<String>,
    pub error: Option<String>,
}

impl Snapshot {
    /// Whether the play button should offer pause. Media3 keeps play-when-ready
    /// set after the queue ends or playback fails; play then starts over or retries.
    pub fn wants_play(&self) -> bool {
        self.play_when_ready && !self.ended && self.error.is_none()
    }
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
    /// (index in the service's queue, track); tracks deleted meanwhile are left out.
    queue: Signal<Vec<(usize, Track)>>,
    /// Bumped twice a second while playing and the app is visible.
    tick: Signal<u64>,
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
            if self.snapshot.peek().playing {
                *tick.write() += 1;
            }
        }
    }

    fn lookup(&self, ids: &[String]) -> Vec<(usize, Track)> {
        let numeric: Vec<(usize, i64)> = ids.iter().enumerate().filter_map(|(i, id)| Some((i, id.parse().ok()?))).collect();
        let tracks = self.library.get().tracks_by_id(&numeric.iter().map(|(_, id)| *id).collect::<Vec<_>>());
        let tracks = tracks.unwrap_or_else(|e| {
            tracing::warn!(target: "ytmdl", "player queue: {e}");
            Vec::new()
        });
        let by_id: std::collections::HashMap<i64, Track> = tracks.into_iter().map(|t| (t.id, t)).collect();
        numeric.into_iter().filter_map(|(i, id)| Some((i, by_id.get(&id)?.clone()))).collect()
    }

    fn save(&self, s: &Snapshot) {
        let saved = Saved {
            ids: s.ids.iter().filter_map(|id| id.parse().ok()).collect(),
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

    /// The queue as YtmdlPlayer.setQueue takes it.
    fn items(&self, tracks: &[Track]) -> String {
        let library = self.library.get();
        let items: Vec<_> = tracks
            .iter()
            .map(|t| {
                let art = t.art.as_deref().and_then(|k| library.art_file(&Library::art_name(k, ART_LARGE)));
                serde_json::json!({
                    "id": t.id.to_string(),
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

    pub fn queue(&self) -> Vec<(usize, Track)> {
        self.queue.read().clone()
    }

    pub fn current(&self) -> Option<Track> {
        let index = self.snapshot.read().index;
        self.queue.read().iter().find(|(i, _)| *i == index).map(|(_, t)| t.clone())
    }

    pub fn current_id(&self) -> Option<i64> {
        let s = self.snapshot.read();
        s.ids.get(s.index).and_then(|id| id.parse().ok())
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
        let s = self.snapshot.read();
        let mut ms = s.position_ms as f64;
        if s.playing {
            ms += self.received.read().elapsed().as_millis() as f64;
        }
        let secs = ms.max(0.0) / 1000.0;
        let duration = if s.duration_ms > 0 { s.duration_ms as f64 / 1000.0 } else { f64::INFINITY };
        secs.min(duration)
    }
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
