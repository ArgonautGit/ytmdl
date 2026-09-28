//! Songs played without downloading them into the library. Their files are in
//! the app's cache folder, which keeps the ones played last up to a size the
//! user sets ([`Library::trim_cache`]). A cached song is a [`Track`] with a
//! negative id (`-<row id>`), so the player's queue keys and the listening log
//! take it like a library song; keeping it copies its file into the library.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use ytmdl_core::Downloaded;
use ytmdl_core::titles::library_path;

use crate::{Library, NewTrack, Result, Track, file_stamp, now, track_from_row};

/// [`crate::TRACK_COLUMNS`] from `cached`, the id negated.
pub(crate) const CACHED_COLUMNS: &str =
    "-id, video_id, url, path, title, artists, album, album_artist, track_number, disc_number, year, duration, art, played_at, gain, peak";

impl Library {
    /// Records a song fetched into the cache. Blocking: reads the file's cover art.
    pub fn add_cached(&self, done: &Downloaded) -> Result<Track> {
        let t = self.downloaded(done)?;
        let sql = format!(
            "INSERT INTO cached (video_id, url, path, title, artists, album, album_artist, track_number,
                                 disc_number, year, duration, art, gain, peak, played_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT (video_id) DO UPDATE SET
                 url = excluded.url, path = excluded.path, title = excluded.title, artists = excluded.artists,
                 album = excluded.album, album_artist = excluded.album_artist, track_number = excluded.track_number,
                 disc_number = excluded.disc_number, year = excluded.year, duration = excluded.duration,
                 art = excluded.art, gain = excluded.gain, peak = excluded.peak, played_at = excluded.played_at
             RETURNING {CACHED_COLUMNS}"
        );
        let params = params![
            t.video_id,
            t.url,
            t.path.to_string_lossy(),
            t.title,
            serde_json::to_string(&t.artists)?,
            t.album,
            t.album_artist,
            t.track_number,
            t.disc_number,
            t.year,
            t.duration_secs,
            t.art,
            t.gain.map(|g| g.gain_db),
            t.gain.map(|g| g.peak),
            now(),
        ];
        Ok(self.db().query_row(&sql, params, track_from_row)?)
    }

    /// The cached song of a video, if its file is still there.
    pub fn cached(&self, video_id: &str) -> Result<Option<Track>> {
        let sql = format!("SELECT {CACHED_COLUMNS} FROM cached WHERE video_id = ?1");
        let track = self.db().query_row(&sql, [video_id], track_from_row).optional()?;
        Ok(track.filter(|t| t.path.exists()))
    }

    /// Marks a cached song (by its negative track id) as just played, so it
    /// is trimmed last.
    pub fn touch_cached(&self, id: i64) -> Result<()> {
        self.db().execute("UPDATE cached SET played_at = ?2 WHERE id = -?1", params![id, now()])?;
        Ok(())
    }

    /// What the cache's files take, in bytes.
    pub fn cache_size(&self) -> Result<u64> {
        Ok(self.cached_files()?.iter().map(|(_, _, size)| size).sum())
    }

    /// Deletes the least recently played cached songs, other than those in
    /// `keep` (track ids: the player's queue), until the rest take at most
    /// `limit` bytes; forgets songs whose files are gone. Blocking. Returns
    /// how many were deleted.
    pub fn trim_cache(&self, limit: u64, keep: &HashSet<i64>) -> Result<usize> {
        let files = self.cached_files()?;
        let mut total: u64 = files.iter().map(|(_, _, size)| size).sum();
        let mut deleted = 0;
        for (id, path, size) in files {
            if total <= limit {
                break;
            }
            if keep.contains(&-id) {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            self.db().execute("DELETE FROM cached WHERE id = ?1", [id])?;
            total -= size;
            deleted += 1;
        }
        Ok(deleted)
    }

    /// Deletes the files in the cache folder `dir` that no cached song has
    /// (left by a fetch that stopped halfway). Only while nothing is fetched.
    pub fn sweep_cache(&self, dir: &Path) -> Result<usize> {
        let known: HashSet<PathBuf> = self.cached_files()?.into_iter().map(|(_, path, _)| path).collect();
        let mut deleted = 0;
        let Ok(entries) = fs::read_dir(dir) else { return Ok(0) };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_file()) && !known.contains(&path) && fs::remove_file(&path).is_ok() {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// (row id, path, size) of the cached songs, the least recently played
    /// first; songs whose files are gone are forgotten.
    fn cached_files(&self) -> Result<Vec<(i64, PathBuf, u64)>> {
        let rows: Vec<(i64, PathBuf)> = {
            let db = self.db();
            let mut stmt = db.prepare("SELECT id, path FROM cached ORDER BY played_at, id")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?))))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut files = Vec::with_capacity(rows.len());
        for (id, path) in rows {
            match fs::metadata(&path) {
                Ok(meta) => files.push((id, path, meta.len())),
                Err(_) => {
                    self.db().execute("DELETE FROM cached WHERE id = ?1", [id])?;
                }
            }
        }
        Ok(files)
    }

    /// Adds a cached song (by its negative track id) to the library: copies
    /// its file to `<music_dir>/<Artist>/<Album>/<Title> [<id>].<ext>`. The
    /// cached copy stays until trimmed, as the player may be playing it.
    /// Blocking. Returns the library's song.
    pub fn keep_cached(&self, id: i64, music_dir: &Path) -> Result<Track> {
        let sql = format!("SELECT {CACHED_COLUMNS} FROM cached WHERE id = -?1");
        let Some(t) = self.db().query_row(&sql, [id], track_from_row).optional()? else {
            return Err(crate::Error::Invalid("that song is no longer cached".into()));
        };
        let ext = t.path.extension().and_then(|e| e.to_str()).unwrap_or("m4a");
        let to = music_dir.join(library_path(&t.album_artist, t.album.as_deref(), &t.title, &t.video_id, ext));
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::copy(&t.path, &to)?;
        let (file_size, file_mtime) = file_stamp(&to)?;
        self.upsert(&NewTrack {
            video_id: t.video_id,
            url: t.url,
            path: to,
            title: t.title,
            artists: t.artists,
            album: t.album,
            album_artist: t.album_artist,
            track_number: t.track_number,
            disc_number: t.disc_number,
            year: t.year,
            duration_secs: t.duration_secs,
            art: t.art,
            gain: t.gain,
            file_size,
            file_mtime,
        })
    }
}
