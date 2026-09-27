//! Lyrics in the Now Playing screen. Downloads store lyrics in the file (see
//! ytmdl_core::lyrics); songs without any are looked up on LRCLIB when their
//! lyrics are opened, and what is found is saved into the file too. The
//! lookups can be turned off in Settings.

use std::collections::HashMap;

use dioxus::prelude::*;
use ytmdl_core::lyrics::{self, Line};
use ytmdl_library::{Library, Track};

use super::Ctx;
use super::views::LyricsView;

/// The Settings switch for LRCLIB lookups ("0" is off).
pub const LOOKUP: &str = "lyrics_lookup";

pub fn lookup_enabled(library: &Library) -> bool {
    library.setting(LOOKUP).ok().flatten().as_deref() != Some("0")
}

/// A song's lyrics as far as the app knows them this run.
#[derive(Clone, PartialEq, Debug)]
pub enum LyricsState {
    /// Reading the file.
    Loading,
    /// Asking LRCLIB.
    Searching,
    Found(Vec<Line>),
    Instrumental,
    /// Not in the file, and not looked up (lookups are off) or not found.
    Missing { searched: bool },
    Failed(String),
}

/// Lyrics per track id.
pub type LyricsCache = HashMap<i64, LyricsState>;

impl LyricsState {
    /// What the lyrics panel shows at `position_ms`.
    pub fn view(&self, position_ms: i64) -> LyricsView {
        match self {
            LyricsState::Loading => LyricsView::Loading,
            LyricsState::Searching => LyricsView::Searching,
            LyricsState::Found(lines) if lines.iter().any(|l| l.at_ms.is_some()) => LyricsView::Synced {
                lines: lines.iter().map(|l| l.text.clone()).collect(),
                current: lyrics::current_line(lines, position_ms),
            },
            LyricsState::Found(lines) => LyricsView::Plain(lines.iter().map(|l| l.text.clone()).collect()),
            LyricsState::Instrumental => LyricsView::Instrumental,
            LyricsState::Missing { searched } => LyricsView::Missing { searched: *searched },
            LyricsState::Failed(e) => LyricsView::Failed(e.clone()),
        }
    }
}

impl Ctx {
    pub(super) fn set_lyrics_lookup(&self, on: bool) {
        if let Err(e) = self.library.get().set_setting(LOOKUP, if on { "1" } else { "0" }) {
            self.notify(format!("Couldn't save the setting: {e}"));
            return;
        }
        let mut lookup = self.lyrics_lookup;
        lookup.set(on);
    }

    /// Reads the track's lyrics from its file, unless they are known already;
    /// looks them up when the file has none and lookups are on.
    pub(super) fn load_lyrics(&self, track: &Track) {
        if self.lyrics.peek().contains_key(&track.id) {
            return;
        }
        let ctx = *self;
        let track = track.clone();
        ctx.set_lyrics_state(track.id, LyricsState::Loading);
        spawn(async move {
            let path = track.path.clone();
            let stored = crate::blocking(move || ytmdl_core::tag::read_lyrics(&path)).await;
            match stored {
                Ok(Ok(Some(text))) => ctx.set_lyrics_state(track.id, LyricsState::Found(lyrics::parse(&text))),
                Ok(Ok(None)) if *ctx.lyrics_lookup.peek() && ctx.services().is_some() => ctx.search_lyrics(track),
                Ok(Ok(None)) => ctx.set_lyrics_state(track.id, LyricsState::Missing { searched: false }),
                Ok(Err(e)) => ctx.set_lyrics_state(track.id, LyricsState::Failed(e.to_string())),
                Err(e) => ctx.set_lyrics_state(track.id, LyricsState::Failed(format!("{e:#}"))),
            }
        });
    }

    /// Looks the track up on LRCLIB and saves what is found into its file.
    pub(super) fn search_lyrics(&self, track: Track) {
        let Some(svc) = self.services() else {
            self.set_lyrics_state(track.id, LyricsState::Missing { searched: false });
            self.notify("The downloader is still starting".into());
            return;
        };
        let ctx = *self;
        ctx.set_lyrics_state(track.id, LyricsState::Searching);
        spawn(async move {
            let song = lyrics::Song {
                title: &track.title,
                artists: &track.artists,
                album: track.album.as_deref(),
                duration_secs: track.duration_secs,
            };
            let found = match svc.dl.lyrics(song).await {
                Ok(found) => found,
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "looking up lyrics for {}: {e}", track.title);
                    return ctx.set_lyrics_state(track.id, LyricsState::Failed("Couldn't reach LRCLIB".into()));
                }
            };
            let Some(found) = found else {
                return ctx.set_lyrics_state(track.id, LyricsState::Missing { searched: true });
            };
            let Some(text) = found.text().map(str::to_owned) else {
                return ctx.set_lyrics_state(track.id, LyricsState::Instrumental);
            };
            ctx.set_lyrics_state(track.id, LyricsState::Found(lyrics::parse(&text)));
            let path = track.path.clone();
            let saved = crate::blocking(move || ytmdl_core::tag::write_lyrics(&path, &text)).await;
            if let Some(e) = match saved {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(e) => Some(format!("{e:#}")),
            } {
                tracing::warn!(target: "ytmdl", "saving lyrics of {}: {e}", track.title);
                ctx.notify("Couldn't save the lyrics in the song's file".into());
            }
        });
    }

    fn set_lyrics_state(&self, id: i64, state: LyricsState) {
        let mut lyrics = self.lyrics;
        lyrics.write().insert(id, state);
    }
}
