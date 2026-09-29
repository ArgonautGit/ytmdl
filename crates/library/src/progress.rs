//! Where a playlist was left: the order it was playing in, the song and the
//! place in it, so "Resume" can carry on, and, for a shuffled playlist, the
//! songs already heard in this round so shuffling again starts with the ones
//! that weren't. Over restarts every song then comes up once before any
//! comes up twice.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Library, Result};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    /// Track ids in the order they were queued.
    pub order: Vec<i64>,
    pub shuffled: bool,
    /// The song playing, as a position in `order`.
    pub index: usize,
    /// Where in that song.
    pub position_ms: i64,
    /// The queue ran out.
    pub done: bool,
    /// Songs already played in this round of shuffling.
    pub heard: Vec<i64>,
    /// `order[..split]` were unheard when the queue was made; the rest were
    /// heard already and follow so the music doesn't stop at the end of a round.
    pub split: usize,
}

impl Progress {
    /// The playlist played in its own order from `start`. The round of
    /// shuffling is left as it was.
    pub fn in_order(tracks: &[i64], start: usize, previous: Option<&Progress>) -> Progress {
        Progress {
            order: tracks.to_vec(),
            index: start.min(tracks.len().saturating_sub(1)),
            heard: previous.map(|p| p.heard.clone()).unwrap_or_default(),
            ..Progress::default()
        }
    }

    /// The playlist shuffled: the songs not heard yet in random order, then
    /// the heard ones (also random). With everything heard a new round begins.
    pub fn shuffled(tracks: &[i64], previous: Option<&Progress>, seed: u64) -> Progress {
        let mut rng = Rng(seed | 1);
        let mut heard: HashSet<i64> = previous.map(|p| p.heard.iter().copied().collect()).unwrap_or_default();
        heard.retain(|id| tracks.contains(id));
        if tracks.iter().all(|id| heard.contains(id)) {
            heard.clear();
        }
        let (mut fresh, mut old): (Vec<i64>, Vec<i64>) = tracks.iter().partition(|id| !heard.contains(id));
        rng.shuffle(&mut fresh);
        rng.shuffle(&mut old);
        let split = fresh.len();
        fresh.extend(old);
        Progress { order: fresh, shuffled: true, heard: sorted(heard), split, ..Progress::default() }
    }

    /// Whether "Resume" has somewhere to go: it isn't at the very start, and
    /// the queue didn't run out.
    pub fn resumable(&self) -> bool {
        !self.done && !self.order.is_empty() && (self.index > 0 || self.position_ms > 0)
    }

    /// The song it was left at.
    pub fn current(&self) -> Option<i64> {
        self.order.get(self.index).copied()
    }

    /// Records what the player reports: `id` playing at `position_ms`, or the
    /// queue having ended. Ignored for a song that isn't in the order (one
    /// queued to play next, say).
    pub fn note(&mut self, id: i64, position_ms: i64, ended: bool) {
        let Some(index) = self.order.iter().position(|&o| o == id) else { return };
        self.index = index;
        self.position_ms = position_ms.max(0);
        self.done = ended;
        if !self.shuffled {
            return;
        }
        // Everything the queue has moved past has been heard, and the song
        // that just ended.
        let passed = index + usize::from(ended);
        let mut heard: HashSet<i64> = self.heard.iter().copied().collect();
        heard.extend(&self.order[..passed]);
        if self.split > 0 && passed >= self.split {
            // The round is over: the unheard songs have all played. What
            // follows starts the next, with only what has played since.
            heard = self.order[..passed].iter().copied().collect();
            self.split = 0;
        }
        self.heard = sorted(heard);
    }

    /// The queue to resume with, brought up to date with the playlist's
    /// `tracks`: songs deleted since are left out, and new ones go among the
    /// songs still to come. `None` if nothing is left to play.
    pub fn resume(&self, tracks: &[i64], seed: u64) -> Option<Progress> {
        if !self.resumable() {
            return None;
        }
        let mut rng = Rng(seed | 1);
        let present: HashSet<i64> = tracks.iter().copied().collect();
        let mut progress = if self.shuffled {
            self.clone()
        } else {
            // A plain playlist plays in its own order, as it is now.
            Progress { order: tracks.to_vec(), ..self.clone() }
        };
        // The saved song, or the first one after it that is still there.
        let Some(&current) = self.order[self.index..].iter().find(|id| present.contains(id)) else { return None };
        let same_song = Some(current) == self.current();
        if self.shuffled {
            let kept_before_split = self.order[..self.split].iter().filter(|id| present.contains(id)).count();
            progress.order.retain(|id| present.contains(id));
            progress.split = kept_before_split;
        }
        progress.index = progress.order.iter().position(|&o| o == current)?;
        progress.position_ms = if same_song { self.position_ms } else { 0 };
        if self.shuffled {
            let known: HashSet<i64> = self.order.iter().copied().collect();
            for &id in tracks.iter().filter(|id| !known.contains(id)) {
                // New songs are unheard: into the part of the queue that is.
                let tail_start = progress.index + 1;
                let in_unheard = tail_start <= progress.split;
                let end = if in_unheard { progress.split } else { progress.order.len() };
                let at = tail_start + rng.below(end - tail_start + 1);
                progress.order.insert(at, id);
                if in_unheard {
                    progress.split += 1;
                }
            }
        }
        Some(progress)
    }
}

fn sorted(set: HashSet<i64>) -> Vec<i64> {
    let mut v: Vec<i64> = set.into_iter().collect();
    v.sort_unstable();
    v
}

/// xorshift, good enough for a play order.
pub struct Rng(pub u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// 0..n
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// Fisher-Yates.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

impl Library {
    pub fn playlist_progress(&self, playlist: i64) -> Result<Option<Progress>> {
        let json: Option<String> =
            self.db().query_row("SELECT state FROM playlist_progress WHERE playlist_id = ?1", [playlist], |r| r.get(0)).optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }

    pub fn set_playlist_progress(&self, playlist: i64, progress: &Progress) -> Result<()> {
        // Not for a playlist deleted meanwhile.
        self.db().execute(
            "INSERT INTO playlist_progress (playlist_id, state) SELECT id, ?2 FROM playlists WHERE id = ?1
             ON CONFLICT (playlist_id) DO UPDATE SET state = excluded.state",
            params![playlist, serde_json::to_string(progress)?],
        )?;
        Ok(())
    }
}
