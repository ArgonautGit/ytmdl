//! Playlists downloaded from links follow their YouTube playlist: they sync
//! when the app starts or comes back to the screen (if the last sync is old;
//! see `use_background_work`) and from the playlist's menu. Songs added on
//! YouTube are downloaded, songs removed there leave the playlist (their files
//! stay), and the order follows YouTube's. Edits made here (songs added to the
//! playlist, songs taken out of it) are kept and never sent to YouTube.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use dioxus::prelude::*;
use ytmdl_core::{CollectionKind, Entry, Resolved};

use super::views::plural;
use super::Ctx;

/// How old a sync can get before the app syncs again on its own.
const STALE_SECS: i64 = 30 * 60;

#[derive(Clone, PartialEq, Debug)]
pub enum SyncState {
    Running,
    Failed(String),
}

pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Ctx {
    /// Saves the YouTube playlist at `url` (already resolved into `entries`)
    /// as a synced playlist, and downloads its songs.
    pub(super) fn save_playlist(&self, url: &str, title: String, entries: Vec<Entry>) {
        let ctx = *self;
        let Some(source) = ytmdl_core::playlist_url(url) else {
            ctx.notify("This link doesn't name a playlist".into());
            return;
        };
        let library = ctx.library.get();
        let name = title.clone();
        // Not tied to the page, which may be left before it's saved.
        dioxus::core::spawn_forever(async move {
            let id = match crate::blocking(move || library.create_synced_playlist(&name, &source)).await {
                Ok(Ok(id)) => id,
                Ok(Err(e)) => return ctx.notify(format!("Couldn't save the playlist: {e}")),
                Err(e) => return ctx.notify(format!("Couldn't save the playlist: {e:#}")),
            };
            match ctx.apply_sync(id, entries, true).await {
                Ok(0) => ctx.notify(format!("Added {title} to your playlists")),
                Ok(n) => ctx.notify(format!("Downloading {} from {title}", plural(n, "song", "songs"))),
                Err(e) => ctx.notify(format!("Couldn't save the playlist: {e}")),
            }
        });
    }

    /// Reads the playlist from YouTube again and follows it. `manual` (from the
    /// menu) also retries failed and cancelled downloads, and reports back.
    pub(super) fn sync_playlist(&self, id: i64, manual: bool) {
        let ctx = *self;
        let Some(svc) = ctx.services() else {
            if manual {
                ctx.notify("The downloader is still starting".into());
            }
            return;
        };
        let playlist = ctx.library.get().playlist(id).ok().flatten();
        let Some((name, url)) = playlist.and_then(|p| Some((p.name, p.source_url?))) else { return };
        let mut syncs = ctx.syncs;
        if syncs.peek().get(&id) == Some(&SyncState::Running) {
            return;
        }
        syncs.write().insert(id, SyncState::Running);
        tracing::info!(target: "ytmdl", "syncing playlist {id} ({name:?}){}", if manual { ", asked for" } else { "" });
        // Not tied to the menu it may come from, which closes before this runs.
        dioxus::core::spawn_forever(async move {
            let result = match svc.dl.resolve_as(&url, Some(CollectionKind::Playlist)).await {
                Ok(Resolved::Collection { entries, .. }) => ctx.apply_sync(id, entries, manual).await,
                Ok(Resolved::Track(_)) => Err("the link no longer leads to a playlist".into()),
                Err(e) => Err(e.to_string()),
            };
            match result {
                Ok(n) => {
                    tracing::info!(target: "ytmdl", "synced playlist {id}: {n} downloads started");
                    syncs.write().remove(&id);
                    if n > 0 {
                        ctx.notify(format!("Downloading {} new in {name}", plural(n, "song", "songs")));
                    } else if manual {
                        ctx.notify(format!("{name} is up to date"));
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "syncing playlist {id}: {e}");
                    if manual {
                        ctx.notify(format!("Couldn't sync {name}: {e}"));
                    }
                    syncs.write().insert(id, SyncState::Failed(e));
                }
            }
        });
    }

    /// Stores YouTube's list and downloads what's missing; returns how many
    /// downloads started.
    async fn apply_sync(&self, id: i64, entries: Vec<Entry>, retry: bool) -> Result<usize, String> {
        let library = self.library.get();
        // An empty answer is more likely a hiccup than an emptied playlist.
        let had = library.playlist(id).ok().flatten().map_or(0, |p| p.tracks + p.pending);
        if entries.is_empty() && had > 0 {
            return Err("YouTube listed no songs".into());
        }
        let video_ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        let missing = crate::blocking(move || library.sync_playlist(id, &video_ids))
            .await
            .map_err(|e| format!("{e:#}"))?
            .map_err(|e| e.to_string())?;
        self.library.changed();
        tracing::debug!(target: "ytmdl", "playlist {id}: YouTube lists {} songs, {} not downloaded", entries.len(), missing.len());
        let missing: HashSet<String> = missing.into_iter().collect();
        let started = entries.into_iter().filter(|e| missing.contains(&e.id)).filter(|e| self.download_entry(e.clone(), retry)).count();
        Ok(started)
    }

    /// Syncs every synced playlist whose last sync is old.
    pub(super) fn sync_stale(&self) {
        let now = unix_now();
        for p in self.library.get().playlists().unwrap_or_default() {
            if p.is_synced() && p.synced_at.is_none_or(|t| now - t >= STALE_SECS) {
                self.sync_playlist(p.id, false);
            }
        }
    }
}

/// "just now", "5 min ago", "3 hr ago", "2 days ago".
pub fn ago(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} hr ago", secs / 3600),
        _ => plural((secs / 86400) as usize, "day ago", "days ago"),
    }
}
