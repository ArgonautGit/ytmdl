//! Listening history, for the stats page and the "most played" sorts.
//!
//! The player runs in its own process (and keeps playing with the app closed),
//! so it can't write here. It appends one line per song it played to a log
//! instead, `<started, unix s>\t<track id>\t<ms played>`, and
//! [`Library::import_listens`] moves the lines into the database.
//!
//! Every listen is kept with the time actually played; one counts as a play
//! once it reaches 30 seconds, or half of a song shorter than a minute.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use rusqlite::params;

use crate::{Library, Result, TRACK_COLUMN_COUNT, TRACK_COLUMNS, Track, track_from_row};

/// Listens of the period with whether each one counts as a play.
const PLAYED: &str = "p AS (
    SELECT l.video_id, l.ms, l.ms >= MIN(30000, COALESCE(t.duration, 60) * 500) AS play
    FROM listens l LEFT JOIN tracks t ON t.video_id = l.video_id
    WHERE l.started_at >= ?1
), s AS (
    SELECT video_id AS vid, SUM(play) AS plays, SUM(ms) AS ms FROM p
    WHERE video_id IS NOT NULL GROUP BY video_id
)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Period {
    /// Today and the 6 days before.
    Week,
    /// Today and the 29 days before.
    Month,
    /// This month and the 11 before.
    Year,
    All,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stats {
    pub listened_ms: i64,
    pub plays: u32,
    /// Different songs and artists played.
    pub songs: u32,
    pub artists: u32,
    pub top_tracks: Vec<TopTrack>,
    pub top_artists: Vec<TopArtist>,
    pub top_albums: Vec<TopAlbum>,
    /// Listening time per day (week, month), month (year) or, over two years
    /// of history, year (all time); oldest first.
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopTrack {
    pub track: Track,
    pub plays: u32,
    pub ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopArtist {
    pub name: String,
    pub plays: u32,
    pub ms: i64,
    pub art: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopAlbum {
    pub title: String,
    pub artist: String,
    pub art: Option<String>,
    pub plays: u32,
    pub ms: i64,
}

/// A day (`day` set), a month (`month` set) or a year of listening, in local time.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub year: i32,
    pub month: Option<u32>,
    pub day: Option<u32>,
    /// 0 is Sunday.
    pub weekday: Option<u32>,
    pub ms: i64,
}

/// All-time plays, for sorting the library by them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayCounts {
    /// By video id.
    pub tracks: HashMap<String, u32>,
    /// By (album artist, album).
    pub albums: HashMap<(String, String), u32>,
    pub artists: HashMap<String, u32>,
}

impl Library {
    /// Moves the player's log (at `log`) into the database; returns how many
    /// listens it held.
    pub fn import_listens(&self, log: &Path) -> Result<usize> {
        // The player appends to `log` whenever a song ends, so it is renamed
        // away before reading; an import that stopped halfway left `taken`.
        let taken = log.with_extension("importing");
        let mut imported = 0;
        for _ in 0..2 {
            if !taken.exists() {
                match fs::rename(log, &taken) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => break,
                    Err(e) => return Err(e.into()),
                }
            }
            imported += self.import_log(&taken)?;
            fs::remove_file(&taken)?;
        }
        Ok(imported)
    }

    fn import_log(&self, path: &Path) -> Result<usize> {
        let text = String::from_utf8_lossy(&fs::read(path)?).into_owned();
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut imported = 0;
        {
            let mut insert = tx.prepare(
                "INSERT INTO listens (video_id, started_at, ms)
                 VALUES ((SELECT video_id FROM tracks WHERE id = ?1), ?2, ?3)",
            )?;
            for line in text.lines() {
                let fields: Vec<&str> = line.split('\t').collect();
                let [started, track, ms] = fields.as_slice() else { continue };
                let (Ok(started), Ok(track), Ok(ms)) = (started.parse::<i64>(), track.parse::<i64>(), ms.parse::<i64>())
                else {
                    continue;
                };
                if ms > 0 {
                    insert.execute(params![track, started, ms])?;
                    imported += 1;
                }
            }
        }
        tx.commit()?;
        Ok(imported)
    }

    /// What was played in `period`, with the top 10 songs and artists and top
    /// 8 albums by plays.
    pub fn stats(&self, period: Period) -> Result<Stats> {
        let db = self.db();
        let since = period_start(&db, period)?;
        let (listened_ms, plays): (i64, u32) = db.query_row(
            &format!("WITH {PLAYED} SELECT COALESCE(SUM(ms), 0), COALESCE(SUM(play), 0) FROM p"),
            [since],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let songs: u32 =
            db.query_row(&format!("WITH {PLAYED} SELECT COUNT(*) FROM s WHERE plays > 0"), [since], |r| r.get(0))?;
        let artists: u32 = db.query_row(
            &format!(
                "WITH {PLAYED} SELECT COUNT(DISTINCT a.value)
                 FROM s JOIN tracks t ON t.video_id = s.vid, json_each(t.artists) a WHERE s.plays > 0"
            ),
            [since],
            |r| r.get(0),
        )?;

        let mut stmt = db.prepare(&format!(
            "WITH {PLAYED} SELECT {TRACK_COLUMNS}, plays, ms FROM tracks JOIN s ON s.vid = tracks.video_id
             WHERE plays > 0 ORDER BY plays DESC, ms DESC, title COLLATE NOCASE LIMIT 10"
        ))?;
        let top_tracks = stmt
            .query_map([since], |r| Ok(TopTrack { track: track_from_row(r)?, plays: r.get(TRACK_COLUMN_COUNT)?, ms: r.get(TRACK_COLUMN_COUNT + 1)? }))?
            .collect::<rusqlite::Result<_>>()?;

        let mut stmt = db.prepare(&format!(
            "WITH {PLAYED} SELECT a.value, SUM(s.plays), SUM(s.ms), MAX(t.art)
             FROM s JOIN tracks t ON t.video_id = s.vid, json_each(t.artists) a
             GROUP BY a.value HAVING SUM(s.plays) > 0
             ORDER BY SUM(s.plays) DESC, SUM(s.ms) DESC, a.value COLLATE NOCASE LIMIT 10"
        ))?;
        let top_artists = stmt
            .query_map([since], |r| Ok(TopArtist { name: r.get(0)?, plays: r.get(1)?, ms: r.get(2)?, art: r.get(3)? }))?
            .collect::<rusqlite::Result<_>>()?;

        let mut stmt = db.prepare(&format!(
            "WITH {PLAYED} SELECT t.album, t.album_artist, MAX(t.art), SUM(s.plays), SUM(s.ms)
             FROM s JOIN tracks t ON t.video_id = s.vid
             WHERE t.album IS NOT NULL AND t.album != ''
             GROUP BY t.album_artist, t.album HAVING SUM(s.plays) > 0
             ORDER BY SUM(s.plays) DESC, SUM(s.ms) DESC, t.album COLLATE NOCASE LIMIT 8"
        ))?;
        let top_albums = stmt
            .query_map([since], |r| {
                Ok(TopAlbum { title: r.get(0)?, artist: r.get(1)?, art: r.get(2)?, plays: r.get(3)?, ms: r.get(4)? })
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        let buckets = buckets(&db, period, since)?;
        Ok(Stats { listened_ms, plays, songs, artists, top_tracks, top_artists, top_albums, buckets })
    }

    pub fn play_counts(&self) -> Result<PlayCounts> {
        let db = self.db();
        let mut counts = PlayCounts::default();
        let mut stmt = db.prepare(&format!("WITH {PLAYED} SELECT vid, plays FROM s WHERE plays > 0"))?;
        for row in stmt.query_map([0], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (id, plays) = row?;
            counts.tracks.insert(id, plays);
        }
        let mut stmt = db.prepare(&format!(
            "WITH {PLAYED} SELECT t.album_artist, t.album, SUM(s.plays) FROM s JOIN tracks t ON t.video_id = s.vid
             WHERE t.album IS NOT NULL GROUP BY t.album_artist, t.album HAVING SUM(s.plays) > 0"
        ))?;
        for row in stmt.query_map([0], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))? {
            let (album, plays) = row?;
            counts.albums.insert(album, plays);
        }
        let mut stmt = db.prepare(&format!(
            "WITH {PLAYED} SELECT a.value, SUM(s.plays) FROM s JOIN tracks t ON t.video_id = s.vid, json_each(t.artists) a
             GROUP BY a.value HAVING SUM(s.plays) > 0"
        ))?;
        for row in stmt.query_map([0], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (name, plays) = row?;
            counts.artists.insert(name, plays);
        }
        Ok(counts)
    }
}

/// Unix seconds where `period` starts: local midnight or the first of the month.
fn period_start(db: &rusqlite::Connection, period: Period) -> rusqlite::Result<i64> {
    let modifiers = match period {
        Period::Week => "'start of day', '-6 days'",
        Period::Month => "'start of day', '-29 days'",
        Period::Year => "'start of month', '-11 months'",
        Period::All => return Ok(0),
    };
    db.query_row(&format!("SELECT CAST(strftime('%s', 'now', 'localtime', {modifiers}, 'utc') AS INTEGER)"), [], |r| {
        r.get(0)
    })
}

fn buckets(db: &rusqlite::Connection, period: Period, since: i64) -> rusqlite::Result<Vec<Bucket>> {
    let days = match period {
        Period::Week => Some(7),
        Period::Month => Some(30),
        Period::Year | Period::All => None,
    };
    if let Some(days) = days {
        let mut stmt = db.prepare(
            "WITH RECURSIVE d(day) AS (
                 SELECT date('now', 'localtime', ?1)
                 UNION ALL SELECT date(day, '+1 day') FROM d WHERE day < date('now', 'localtime')
             ), l AS (
                 SELECT date(started_at, 'unixepoch', 'localtime') AS day, SUM(ms) AS ms
                 FROM listens WHERE started_at >= ?2 GROUP BY 1
             )
             SELECT CAST(strftime('%Y', d.day) AS INTEGER), CAST(strftime('%m', d.day) AS INTEGER),
                    CAST(strftime('%d', d.day) AS INTEGER), CAST(strftime('%w', d.day) AS INTEGER), COALESCE(l.ms, 0)
             FROM d LEFT JOIN l ON l.day = d.day ORDER BY d.day",
        )?;
        let rows = stmt.query_map(params![format!("-{} days", days - 1), since], |r| {
            Ok(Bucket { year: r.get(0)?, month: Some(r.get(1)?), day: Some(r.get(2)?), weekday: Some(r.get(3)?), ms: r.get(4)? })
        })?;
        return rows.collect();
    }

    // All time starts at the first listen (this month without any).
    let from = match period {
        Period::All => db.query_row("SELECT MIN(started_at) FROM listens", [], |r| r.get::<_, Option<i64>>(0))?,
        _ => Some(since),
    };
    let mut stmt = db.prepare(
        "WITH RECURSIVE m(month) AS (
             SELECT date(COALESCE(?1, CAST(strftime('%s', 'now') AS INTEGER)), 'unixepoch', 'localtime', 'start of month')
             UNION ALL SELECT date(month, '+1 month') FROM m WHERE month < date('now', 'localtime', 'start of month')
         ), l AS (
             SELECT date(started_at, 'unixepoch', 'localtime', 'start of month') AS month, SUM(ms) AS ms
             FROM listens WHERE started_at >= ?2 GROUP BY 1
         )
         SELECT CAST(strftime('%Y', m.month) AS INTEGER), CAST(strftime('%m', m.month) AS INTEGER), COALESCE(l.ms, 0)
         FROM m LEFT JOIN l ON l.month = m.month ORDER BY m.month",
    )?;
    let months: Vec<Bucket> = stmt
        .query_map(params![from, since], |r| {
            Ok(Bucket { year: r.get(0)?, month: Some(r.get(1)?), day: None, weekday: None, ms: r.get(2)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    if months.len() <= 24 {
        return Ok(months);
    }
    let mut years: Vec<Bucket> = Vec::new();
    for m in months {
        match years.last_mut() {
            Some(y) if y.year == m.year => y.ms += m.ms,
            _ => years.push(Bucket { month: None, ..m }),
        }
    }
    Ok(years)
}
