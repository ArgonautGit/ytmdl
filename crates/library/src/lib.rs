//! The music library: an SQLite index of downloaded tracks and of the download
//! queue, plus playlists, listening history, saved A-B sections and a cache of
//! resized cover art.
//!
//! The audio files stay the record. Each carries its source URL in the comment
//! tag, so [`Library::scan`] can rebuild the index from the music folders, and it
//! drops tracks whose files were deleted elsewhere.

mod art;
mod listens;
mod playlists;
mod scan;
mod sections;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use ytmdl_core::loudness::Gain;
use ytmdl_core::{Downloaded, Entry};

pub use art::{ART_LARGE, ART_SMALL};
use art::ArtCache;
pub use listens::{Bucket, Period, PlayCounts, Stats, TopAlbum, TopArtist, TopTrack};
pub use playlists::{Playlist, PlaylistEntry};
pub use scan::ScanReport;
pub use sections::Section;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("library database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Schema versions, applied in order and tracked in `PRAGMA user_version`.
/// The player's process also reads `tracks`, `playlists`, `playlist_tracks`
/// and the saved queue, for cars (app/android/Browse.kt), so columns there
/// are only ever added.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE tracks (
    id           INTEGER PRIMARY KEY,
    video_id     TEXT NOT NULL UNIQUE,
    url          TEXT NOT NULL,
    path         TEXT NOT NULL UNIQUE,
    title        TEXT NOT NULL,
    artists      TEXT NOT NULL,             -- JSON array
    album        TEXT,
    album_artist TEXT NOT NULL,             -- groups albums; the first artist if untagged
    track_number INTEGER,
    disc_number  INTEGER,
    year         INTEGER,
    duration     REAL,
    art          TEXT,                      -- art cache key
    file_size    INTEGER NOT NULL,
    file_mtime   INTEGER NOT NULL,
    added_at     INTEGER NOT NULL           -- unix seconds
);
CREATE INDEX tracks_album ON tracks (album_artist, album);
CREATE INDEX tracks_added ON tracks (added_at);

CREATE TABLE downloads (
    id         INTEGER PRIMARY KEY,
    entry      TEXT NOT NULL,               -- JSON of ytmdl_core::Entry
    state      TEXT NOT NULL,               -- pending | done | failed | cancelled
    detail     TEXT,                        -- file path, or the error
    tagged     INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
"#, r#"
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#, r#"
CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE playlist_tracks (
    id          INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    position    INTEGER NOT NULL
);
CREATE INDEX playlist_tracks_order ON playlist_tracks (playlist_id, position);
CREATE INDEX playlist_tracks_track ON playlist_tracks (track_id);
"#, r#"
ALTER TABLE playlists ADD COLUMN source_url TEXT;   -- YouTube playlist a synced playlist follows
ALTER TABLE playlists ADD COLUMN synced_at INTEGER; -- unix seconds
CREATE UNIQUE INDEX playlists_source ON playlists (source_url);
-- A synced playlist as YouTube last listed it; its downloaded songs are its
-- playlist_tracks, in this order.
CREATE TABLE playlist_remote (
    playlist_id INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    video_id    TEXT NOT NULL,
    skipped     INTEGER NOT NULL DEFAULT 0,     -- deleted here: not downloaded again
    PRIMARY KEY (playlist_id, position)
);
CREATE INDEX playlist_remote_video ON playlist_remote (video_id);
"#, r#"
-- What was played, from the player's log (see listens.rs). By video id, so
-- a song downloaded again keeps its history.
CREATE TABLE listens (
    id         INTEGER PRIMARY KEY,
    video_id   TEXT,                        -- NULL: deleted before the log was read
    started_at INTEGER NOT NULL,            -- unix seconds
    ms         INTEGER NOT NULL             -- time actually played
);
CREATE INDEX listens_started ON listens (started_at);
CREATE INDEX listens_video ON listens (video_id);
-- Named A-B loops over part of a song.
CREATE TABLE sections (
    id         INTEGER PRIMARY KEY,
    video_id   TEXT NOT NULL,
    name       TEXT NOT NULL,
    a_ms       INTEGER NOT NULL,
    b_ms       INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX sections_video ON sections (video_id, a_ms);
"#, r#"
-- ReplayGain, for volume normalization (see ytmdl_core::loudness): from the
-- file's tags, or measured by `Library::measure_loudness` for files without.
ALTER TABLE tracks ADD COLUMN gain REAL;                     -- dB
ALTER TABLE tracks ADD COLUMN peak REAL;
ALTER TABLE tracks ADD COLUMN measured INTEGER NOT NULL DEFAULT 0; -- gain known, or not measurable
"#];

pub(crate) const TRACK_COLUMNS: &str =
    "id, video_id, url, path, title, artists, album, album_artist, track_number, disc_number, year, duration, art, added_at, gain, peak";
/// How many columns [`TRACK_COLUMNS`] has; queries add theirs after them.
pub(crate) const TRACK_COLUMN_COUNT: usize = 16;

/// A downloaded song.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: i64,
    pub video_id: String,
    pub url: String,
    pub path: PathBuf,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: String,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<i32>,
    pub duration_secs: Option<f64>,
    /// Cover art cache key; see [`Library::art_name`].
    pub art: Option<String>,
    pub added_at: i64,
    /// ReplayGain, once known.
    pub gain: Option<Gain>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    pub art: Option<String>,
    pub tracks: u32,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artist {
    pub name: String,
    pub tracks: u32,
    pub albums: u32,
    pub art: Option<String>,
}

/// A download as saved across restarts.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedDownload {
    pub id: i64,
    pub entry: Entry,
    pub state: SavedState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SavedState {
    /// Queued or running when last saved; resumes on the next start.
    Pending,
    Done { path: PathBuf, tagged: bool },
    Failed(String),
    Cancelled,
}

/// What to store for one track, from a download or from a file's tags.
#[derive(Debug, Clone)]
struct NewTrack {
    video_id: String,
    url: String,
    path: PathBuf,
    title: String,
    artists: Vec<String>,
    album: Option<String>,
    album_artist: String,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    year: Option<i32>,
    duration_secs: Option<f64>,
    art: Option<String>,
    gain: Option<Gain>,
    file_size: u64,
    file_mtime: i64,
}

/// Shared handle; clones use the same connection.
#[derive(Clone)]
pub struct Library(Arc<Inner>);

struct Inner {
    db: Mutex<Connection>,
    art: ArtCache,
}

impl Library {
    pub fn open(db: &Path, art_dir: &Path) -> Result<Library> {
        if let Some(parent) = db.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn, art_dir)
    }

    /// A throwaway library (tests and previews).
    pub fn open_in_memory(art_dir: &Path) -> Result<Library> {
        Self::init(Connection::open_in_memory()?, art_dir)
    }

    fn init(mut conn: Connection, art_dir: &Path) -> Result<Library> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        Ok(Library(Arc::new(Inner {
            db: Mutex::new(conn),
            art: ArtCache::new(art_dir.to_owned()),
        })))
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves nothing half-written (SQLite rolls back).
        self.0.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- tracks ----

    /// Records a finished download. Blocking: reads the file's cover art and
    /// writes resized copies, so call it off the UI thread.
    pub fn add_download(&self, done: &Downloaded) -> Result<Track> {
        let meta = &done.meta;
        let (file_size, file_mtime) = file_stamp(&done.path)?;
        let art = scan::read_cover(&done.path).and_then(|data| self.0.art.store(&data));
        let gain = ytmdl_core::loudness::read(&done.path).ok().flatten();
        let album_artist = meta
            .album_artists
            .first()
            .map(|_| meta.album_artists.join(", "))
            .or_else(|| meta.artists.first().cloned())
            .unwrap_or_default();
        self.upsert(&NewTrack {
            video_id: meta.id.clone(),
            url: meta.url.clone(),
            path: done.path.clone(),
            title: meta.title.clone(),
            artists: meta.artists.clone(),
            album: meta.album.clone(),
            album_artist,
            track_number: meta.track_number,
            disc_number: meta.disc_number,
            year: meta.year,
            duration_secs: meta.duration_secs,
            art,
            gain,
            file_size,
            file_mtime,
        })
    }

    fn upsert(&self, t: &NewTrack) -> Result<Track> {
        let db = self.db();
        let path = t.path.to_string_lossy();
        // Another video at the same path (re-downloaded under a new id) is replaced.
        db.execute("DELETE FROM tracks WHERE path = ?1 AND video_id != ?2", params![path, t.video_id])?;
        let sql = format!(
            "INSERT INTO tracks (video_id, url, path, title, artists, album, album_artist, track_number,
                                 disc_number, year, duration, art, file_size, file_mtime, added_at,
                                 gain, peak, measured)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?16 IS NOT NULL)
             ON CONFLICT (video_id) DO UPDATE SET
                 url = excluded.url, path = excluded.path, title = excluded.title,
                 artists = excluded.artists, album = excluded.album, album_artist = excluded.album_artist,
                 track_number = excluded.track_number, disc_number = excluded.disc_number,
                 year = excluded.year, duration = excluded.duration, art = excluded.art,
                 file_size = excluded.file_size, file_mtime = excluded.file_mtime,
                 gain = excluded.gain, peak = excluded.peak, measured = excluded.measured
             RETURNING {TRACK_COLUMNS}"
        );
        let track = db.query_row(
            &sql,
            params![
                t.video_id,
                t.url,
                path,
                t.title,
                serde_json::to_string(&t.artists)?,
                t.album,
                t.album_artist,
                t.track_number,
                t.disc_number,
                t.year,
                t.duration_secs,
                t.art,
                t.file_size as i64,
                t.file_mtime,
                now(),
                t.gain.map(|g| g.gain_db),
                t.gain.map(|g| g.peak),
            ],
            track_from_row,
        )?;
        playlists::place_in_synced(&db, &track)?;
        Ok(track)
    }

    /// Every track, most recently added first.
    pub fn tracks(&self) -> Result<Vec<Track>> {
        self.query_tracks(&format!("SELECT {TRACK_COLUMNS} FROM tracks ORDER BY added_at DESC, id DESC"), [])
    }

    pub fn track(&self, id: i64) -> Result<Option<Track>> {
        let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE id = ?1");
        Ok(self.db().query_row(&sql, [id], track_from_row).optional()?)
    }

    /// Tracks in the order of `ids`, skipping ids that are gone.
    pub fn tracks_by_id(&self, ids: &[i64]) -> Result<Vec<Track>> {
        let db = self.db();
        let mut stmt = db.prepare_cached(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE id = ?1"))?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = stmt.query_row([id], track_from_row).optional()? {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Albums, most recently added to first.
    pub fn albums(&self) -> Result<Vec<Album>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT album, album_artist, MAX(year), MAX(art), COUNT(*), TOTAL(duration)
             FROM tracks WHERE album IS NOT NULL AND album != ''
             GROUP BY album_artist, album
             ORDER BY MAX(added_at) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Album {
                title: r.get(0)?,
                artist: r.get(1)?,
                year: r.get(2)?,
                art: r.get(3)?,
                tracks: r.get(4)?,
                duration_secs: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// An album's tracks in album order.
    pub fn album_tracks(&self, title: &str, artist: &str) -> Result<Vec<Track>> {
        self.query_tracks(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks WHERE album = ?1 AND album_artist = ?2
                 ORDER BY COALESCE(disc_number, 1), track_number IS NULL, track_number, added_at, id"
            ),
            params![title, artist],
        )
    }

    /// Everyone credited on a track, by name.
    pub fn artists(&self) -> Result<Vec<Artist>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT a.value, COUNT(DISTINCT t.id), COUNT(DISTINCT t.album), MAX(t.art)
             FROM tracks t, json_each(t.artists) a
             GROUP BY a.value
             ORDER BY a.value COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Artist { name: r.get(0)?, tracks: r.get(1)?, albums: r.get(2)?, art: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// An artist's tracks, grouped by album.
    pub fn artist_tracks(&self, name: &str) -> Result<Vec<Track>> {
        self.query_tracks(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks t
                 WHERE EXISTS (SELECT 1 FROM json_each(t.artists) WHERE value = ?1)
                 ORDER BY album IS NULL, album COLLATE NOCASE, COALESCE(disc_number, 1),
                          track_number IS NULL, track_number, title COLLATE NOCASE"
            ),
            [name],
        )
    }

    /// Video ids of every track, to mark search results that are already here.
    pub fn video_ids(&self) -> Result<HashSet<String>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT video_id FROM tracks")?;
        let ids = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Forgets a track (and its playlist entries) and deletes its file, then the
    /// album and artist folders if that left them empty. Synced playlists won't
    /// download it again. The cover art stays cached (other tracks of the album
    /// share it). Returns the deleted track.
    pub fn delete_track(&self, id: i64) -> Result<Option<Track>> {
        let Some(track) = self.track(id)? else { return Ok(None) };
        match fs::remove_file(&track.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let db = self.db();
        db.execute("UPDATE playlist_remote SET skipped = 1 WHERE video_id = ?1", [&track.video_id])?;
        db.execute("DELETE FROM tracks WHERE id = ?1", [id])?;
        drop(db);
        // `<Artist>/<Album>/<file>`: remove_dir only succeeds on empty folders.
        for dir in track.path.ancestors().skip(1).take(2) {
            if fs::remove_dir(dir).is_err() {
                break;
            }
        }
        Ok(Some(track))
    }

    fn query_tracks(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Track>> {
        let db = self.db();
        let mut stmt = db.prepare(sql)?;
        let rows = stmt.query_map(params, track_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- loudness ----

    /// Tracks whose ReplayGain is unknown and not yet measured, newest first.
    pub fn unmeasured(&self) -> Result<Vec<Track>> {
        self.query_tracks(
            &format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE NOT measured ORDER BY added_at DESC, id DESC"),
            [],
        )
    }

    /// Records a track's measured gain (`None`: its file can't be measured,
    /// so it isn't tried again). A file that now holds the gain in its tags
    /// gets its new size and time recorded, so a scan doesn't read it again.
    pub fn set_gain(&self, id: i64, gain: Option<Gain>, retagged: bool) -> Result<()> {
        let db = self.db();
        db.execute(
            "UPDATE tracks SET gain = ?2, peak = ?3, measured = 1 WHERE id = ?1",
            params![id, gain.map(|g| g.gain_db), gain.map(|g| g.peak)],
        )?;
        if retagged {
            let path: String = db.query_row("SELECT path FROM tracks WHERE id = ?1", [id], |r| r.get(0))?;
            let (size, mtime) = file_stamp(Path::new(&path))?;
            db.execute(
                "UPDATE tracks SET file_size = ?2, file_mtime = ?3 WHERE id = ?1",
                params![id, size as i64, mtime],
            )?;
        }
        Ok(())
    }

    // ---- cover art ----

    /// File name of a cached cover at `size` ([`ART_SMALL`] or [`ART_LARGE`]).
    pub fn art_name(key: &str, size: u32) -> String {
        ArtCache::name(key, size)
    }

    /// Path of a cached cover by the name from [`Library::art_name`], if it exists.
    pub fn art_file(&self, name: &str) -> Option<PathBuf> {
        self.0.art.file(name)
    }

    // ---- settings ----

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db().query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- downloads ----

    pub fn add_download_job(&self, entry: &Entry) -> Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO downloads (entry, state, created_at) VALUES (?1, 'pending', ?2)",
            params![serde_json::to_string(entry)?, now()],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn set_download_state(&self, id: i64, state: &SavedState) -> Result<()> {
        let (name, detail, tagged) = match state {
            SavedState::Pending => ("pending", None, false),
            SavedState::Done { path, tagged } => ("done", Some(path.to_string_lossy().into_owned()), *tagged),
            SavedState::Failed(e) => ("failed", Some(e.clone()), false),
            SavedState::Cancelled => ("cancelled", None, false),
        };
        self.db().execute(
            "UPDATE downloads SET state = ?2, detail = ?3, tagged = ?4 WHERE id = ?1",
            params![id, name, detail, tagged],
        )?;
        Ok(())
    }

    /// Saved downloads, oldest first.
    pub fn download_jobs(&self) -> Result<Vec<SavedDownload>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, entry, state, detail, tagged FROM downloads ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, entry, state, detail, tagged) = row?;
            let entry = match serde_json::from_str(&entry) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "skipping unreadable download {id}: {e}");
                    continue;
                }
            };
            let state = match state.as_str() {
                "done" => SavedState::Done { path: detail.unwrap_or_default().into(), tagged },
                "failed" => SavedState::Failed(detail.unwrap_or_default()),
                "cancelled" => SavedState::Cancelled,
                _ => SavedState::Pending,
            };
            out.push(SavedDownload { id, entry, state });
        }
        Ok(out)
    }

    /// Forgets every download that is not pending.
    pub fn remove_finished_jobs(&self) -> Result<usize> {
        Ok(self.db().execute("DELETE FROM downloads WHERE state != 'pending'", [])?)
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let version = version as usize;
    if version > MIGRATIONS.len() {
        return Err(Error::Invalid(format!(
            "the library was written by a newer ytmdl (schema {version}, this one knows {})",
            MIGRATIONS.len()
        )));
    }
    let tx = conn.transaction()?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as u32 + 1)?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn track_from_row(r: &Row) -> rusqlite::Result<Track> {
    let artists: String = r.get(5)?;
    Ok(Track {
        id: r.get(0)?,
        video_id: r.get(1)?,
        url: r.get(2)?,
        path: PathBuf::from(r.get::<_, String>(3)?),
        title: r.get(4)?,
        artists: serde_json::from_str(&artists).unwrap_or_default(),
        album: r.get(6)?,
        album_artist: r.get(7)?,
        track_number: r.get(8)?,
        disc_number: r.get(9)?,
        year: r.get(10)?,
        duration_secs: r.get(11)?,
        art: r.get(12)?,
        added_at: r.get(13)?,
        gain: r.get::<_, Option<f64>>(14)?.map(|gain_db| Gain { gain_db, peak: r.get::<_, Option<f64>>(15).ok().flatten().unwrap_or(1.0) }),
    })
}

/// (size, mtime) to notice a file changed since it was indexed.
fn file_stamp(path: &Path) -> Result<(u64, i64)> {
    let meta = fs::metadata(path)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    Ok((meta.len(), mtime))
}

pub(crate) fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests;
