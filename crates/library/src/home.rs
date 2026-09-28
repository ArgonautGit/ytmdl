//! What the Home tab shows: the songs played lately and most, favourites not
//! played in a while, and the albums added last.

use rusqlite::params;

use crate::listens::{PLAYED, period_start};
use crate::{Album, Library, Period, Result, TRACK_COLUMNS, Track, track_from_row};

/// Songs in each list, at most.
const SONGS: i64 = 20;
/// Albums in the latest added.
const ALBUMS: usize = 12;
/// Plays that make a song not played this month a forgotten favourite.
const FAVOURITE_PLAYS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Home {
    /// Songs played lately (at least as long as a play counts), the last first.
    pub recent: Vec<Track>,
    /// The most played songs of the last 30 days.
    pub top: Vec<Track>,
    /// Songs played often but not in the last 30 days, the most played first.
    pub forgotten: Vec<Track>,
    /// The albums added to last.
    pub added: Vec<Album>,
    /// The hour, 0 to 23, in local time.
    pub hour: u32,
}

impl Library {
    pub fn home(&self) -> Result<Home> {
        let added = self.albums()?.into_iter().take(ALBUMS).collect();
        let db = self.db();
        let month = period_start(&db, Period::Month)?;
        let tracks = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Result<Vec<Track>> {
            let mut stmt = db.prepare(sql)?;
            let rows = stmt.query_map(params, track_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        // A listen counts as a play as in PLAYED.
        let recent = tracks(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks JOIN (
                     SELECT l.video_id AS vid, MAX(l.started_at) AS last
                     FROM listens l JOIN tracks t ON t.video_id = l.video_id
                     WHERE l.ms >= MIN(30000, COALESCE(t.duration, 60) * 500) GROUP BY l.video_id
                 ) r ON r.vid = tracks.video_id
                 ORDER BY r.last DESC, tracks.id DESC LIMIT ?1"
            ),
            params![SONGS],
        )?;
        let top = tracks(
            &format!(
                "WITH {PLAYED} SELECT {TRACK_COLUMNS} FROM tracks JOIN s ON s.vid = tracks.video_id
                 WHERE plays > 0 ORDER BY plays DESC, ms DESC, tracks.id DESC LIMIT ?2"
            ),
            params![month, SONGS],
        )?;
        let forgotten = tracks(
            &format!(
                "WITH {PLAYED}, last AS (
                     SELECT video_id AS lv, MAX(started_at) AS at FROM listens WHERE video_id IS NOT NULL GROUP BY video_id
                 )
                 SELECT {TRACK_COLUMNS} FROM tracks JOIN s ON s.vid = tracks.video_id JOIN last ON last.lv = tracks.video_id
                 WHERE plays >= ?2 AND last.at < ?3 ORDER BY plays DESC, ms DESC, tracks.id DESC LIMIT ?4"
            ),
            params![0, FAVOURITE_PLAYS, month, SONGS],
        )?;
        let hour = db.query_row("SELECT CAST(strftime('%H', 'now', 'localtime') AS INTEGER)", [], |r| r.get(0))?;
        Ok(Home { recent, top, forgotten, added, hour })
    }
}
