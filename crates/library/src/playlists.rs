//! User playlists. Entries reference tracks, so deleting a track (or a scan
//! finding its file gone) takes it out of every playlist too.
//!
//! A synced playlist follows a YouTube playlist: [`Library::sync_playlist`]
//! stores YouTube's list, and its entries are the songs of that list that are
//! downloaded, in YouTube's order. Songs join it as their downloads finish.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Library, Result, TRACK_COLUMN_COUNT, TRACK_COLUMNS, Track, now, track_from_row};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub tracks: u32,
    pub duration_secs: f64,
    /// Cover art key of the first song that has one.
    pub art: Option<String>,
    /// The YouTube playlist a synced playlist follows.
    pub source_url: Option<String>,
    /// Unix seconds of the last sync.
    pub synced_at: Option<i64>,
    /// Songs of the YouTube playlist it should have (not the ones deleted
    /// here); more than `tracks` while some are still to download.
    pub wanted: u32,
}

impl Playlist {
    pub fn is_synced(&self) -> bool {
        self.source_url.is_some()
    }
}

/// A song in a playlist; the same track can be in several.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub entry_id: i64,
    pub track: Track,
}

const PLAYLIST_COLUMNS: &str = "p.id, p.name,
    (SELECT COUNT(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id),
    (SELECT TOTAL(t.duration) FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id WHERE pt.playlist_id = p.id),
    (SELECT t.art FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
     WHERE pt.playlist_id = p.id AND t.art IS NOT NULL ORDER BY pt.position LIMIT 1),
    p.source_url, p.synced_at,
    (SELECT COUNT(DISTINCT r.video_id) FROM playlist_remote r WHERE r.playlist_id = p.id AND NOT r.skipped)";

fn playlist_from_row(r: &rusqlite::Row) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: r.get(0)?,
        name: r.get(1)?,
        tracks: r.get(2)?,
        duration_secs: r.get(3)?,
        art: r.get(4)?,
        source_url: r.get(5)?,
        synced_at: r.get(6)?,
        wanted: r.get(7)?,
    })
}

/// Adds a track that was just recorded to the synced playlists that list it,
/// in their order (and lets them download it again if it was deleted before).
pub(crate) fn place_in_synced(db: &Connection, track: &Track) -> rusqlite::Result<()> {
    db.execute("UPDATE playlist_remote SET skipped = 0 WHERE video_id = ?1 AND skipped", [&track.video_id])?;
    db.execute(
        "INSERT INTO playlist_tracks (playlist_id, track_id, position)
         SELECT r.playlist_id, ?2, MIN(r.position) FROM playlist_remote r
         WHERE r.video_id = ?1
           AND NOT EXISTS (SELECT 1 FROM playlist_tracks pt WHERE pt.playlist_id = r.playlist_id AND pt.track_id = ?2)
         GROUP BY r.playlist_id",
        params![track.video_id, track.id],
    )?;
    Ok(())
}

impl Library {
    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        let db = self.db();
        db.execute("INSERT INTO playlists (name, created_at) VALUES (?1, ?2)", params![name.trim(), now()])?;
        Ok(db.last_insert_rowid())
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<()> {
        self.db().execute("UPDATE playlists SET name = ?2 WHERE id = ?1", params![id, name.trim()])?;
        Ok(())
    }

    /// Deletes the playlist, not its songs.
    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Playlists, newest first.
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let db = self.db();
        let mut stmt = db.prepare(&format!("SELECT {PLAYLIST_COLUMNS} FROM playlists p ORDER BY p.created_at DESC, p.id DESC"))?;
        let rows = stmt.query_map([], playlist_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn playlist(&self, id: i64) -> Result<Option<Playlist>> {
        let sql = format!("SELECT {PLAYLIST_COLUMNS} FROM playlists p WHERE p.id = ?1");
        Ok(self.db().query_row(&sql, [id], playlist_from_row).optional()?)
    }

    pub fn playlist_tracks(&self, id: i64) -> Result<Vec<PlaylistEntry>> {
        let columns = TRACK_COLUMNS.split(", ").map(|c| format!("t.{c}")).collect::<Vec<_>>().join(", ");
        let db = self.db();
        let mut stmt = db.prepare(&format!(
            "SELECT {columns}, pt.id FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
             WHERE pt.playlist_id = ?1 ORDER BY pt.position, pt.id"
        ))?;
        let rows = stmt.query_map([id], |r| Ok(PlaylistEntry { track: track_from_row(r)?, entry_id: r.get(TRACK_COLUMN_COUNT)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Appends the tracks that aren't in the playlist yet; returns how many.
    pub fn add_to_playlist(&self, id: i64, track_ids: &[i64]) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut added = 0;
        {
            let mut position: i64 = tx.query_row(
                "SELECT COALESCE(MAX(position), 0) FROM playlist_tracks WHERE playlist_id = ?1",
                [id],
                |r| r.get(0),
            )?;
            let mut exists = tx.prepare("SELECT 1 FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2")?;
            let mut insert =
                tx.prepare("INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (?1, ?2, ?3)")?;
            for &track in track_ids {
                if exists.exists(params![id, track])? {
                    continue;
                }
                position += 1;
                insert.execute(params![id, track, position])?;
                added += 1;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    pub fn remove_from_playlist(&self, entry_id: i64) -> Result<()> {
        self.db().execute("DELETE FROM playlist_tracks WHERE id = ?1", [entry_id])?;
        Ok(())
    }

    // ---- synced playlists ----

    /// The synced playlist following `source_url`, if there is one.
    pub fn synced_playlist(&self, source_url: &str) -> Result<Option<i64>> {
        let db = self.db();
        Ok(db.query_row("SELECT id FROM playlists WHERE source_url = ?1", [source_url], |r| r.get(0)).optional()?)
    }

    /// A playlist following `source_url` (the existing one if there is one);
    /// [`Library::sync_playlist`] fills it.
    pub fn create_synced_playlist(&self, name: &str, source_url: &str) -> Result<i64> {
        if let Some(id) = self.synced_playlist(source_url)? {
            return Ok(id);
        }
        let db = self.db();
        db.execute(
            "INSERT INTO playlists (name, created_at, source_url) VALUES (?1, ?2, ?3)",
            params![name.trim(), now(), source_url],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Makes a synced playlist match YouTube's list (`video_ids`, in order):
    /// downloaded songs of the list join it or move to their place, songs no
    /// longer listed leave it (their files stay). Returns the listed songs
    /// that aren't downloaded, in order, except ones deleted here.
    pub fn sync_playlist(&self, id: i64, video_ids: &[String]) -> Result<Vec<String>> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let skipped: HashSet<String> = {
            let mut stmt = tx.prepare("SELECT video_id FROM playlist_remote WHERE playlist_id = ?1 AND skipped")?;
            stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        tx.execute("DELETE FROM playlist_remote WHERE playlist_id = ?1", [id])?;
        {
            let mut insert =
                tx.prepare("INSERT INTO playlist_remote (playlist_id, position, video_id, skipped) VALUES (?1, ?2, ?3, ?4)")?;
            for (position, video_id) in video_ids.iter().enumerate() {
                insert.execute(params![id, position as i64, video_id, skipped.contains(video_id)])?;
            }
        }
        // Update the entries in place, so the ones that stay keep their ids.
        let listed = "SELECT t.id FROM playlist_remote r JOIN tracks t ON t.video_id = r.video_id WHERE r.playlist_id = ?1";
        tx.execute(&format!("DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND track_id NOT IN ({listed})"), [id])?;
        tx.execute(
            "UPDATE playlist_tracks SET position = (
                 SELECT MIN(r.position) FROM playlist_remote r JOIN tracks t ON t.video_id = r.video_id
                 WHERE r.playlist_id = ?1 AND t.id = playlist_tracks.track_id)
             WHERE playlist_id = ?1",
            [id],
        )?;
        tx.execute(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position)
             SELECT ?1, t.id, MIN(r.position) FROM playlist_remote r JOIN tracks t ON t.video_id = r.video_id
             WHERE r.playlist_id = ?1 AND t.id NOT IN (SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1)
             GROUP BY t.id",
            [id],
        )?;
        tx.execute("UPDATE playlists SET synced_at = ?2 WHERE id = ?1", params![id, now()])?;
        let missing = {
            let mut stmt = tx.prepare(
                "SELECT r.video_id FROM playlist_remote r
                 WHERE r.playlist_id = ?1 AND NOT r.skipped
                   AND NOT EXISTS (SELECT 1 FROM tracks t WHERE t.video_id = r.video_id)
                 GROUP BY r.video_id ORDER BY MIN(r.position)",
            )?;
            stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?
        };
        tx.commit()?;
        Ok(missing)
    }

    /// Turns a synced playlist into a plain one holding the songs it has now.
    pub fn stop_syncing(&self, id: i64) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("UPDATE playlists SET source_url = NULL, synced_at = NULL WHERE id = ?1", [id])?;
        tx.execute("DELETE FROM playlist_remote WHERE playlist_id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }
}
