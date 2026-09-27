//! User playlists. Entries reference tracks, so deleting a track (or a scan
//! finding its file gone) takes it out of every playlist too.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Library, Result, TRACK_COLUMNS, Track, now, track_from_row};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub tracks: u32,
    pub duration_secs: f64,
    /// Cover art key of the first song that has one.
    pub art: Option<String>,
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
     WHERE pt.playlist_id = p.id AND t.art IS NOT NULL ORDER BY pt.position LIMIT 1)";

fn playlist_from_row(r: &rusqlite::Row) -> rusqlite::Result<Playlist> {
    Ok(Playlist { id: r.get(0)?, name: r.get(1)?, tracks: r.get(2)?, duration_secs: r.get(3)?, art: r.get(4)? })
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
        let rows = stmt.query_map([id], |r| Ok(PlaylistEntry { track: track_from_row(r)?, entry_id: r.get(14)? }))?;
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
}
