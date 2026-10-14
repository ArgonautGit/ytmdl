//! The listening history for listening insights: a summary of it, read-only
//! SQL over it (a model asks its own questions of it), and an export.
//!
//! Queries run on a connection of their own, opened read-only, over views
//! that put each listen's local time and play next to its song:
//!
//! - `history`: one row per listen (see [`VIEWS`]).
//! - `library`: the downloaded songs.
//! - `playlist_songs`: each playlist's songs, in order.

use std::fs;
use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::{Error, Library, Result};

/// The views queries see, described for the model in `VIEW_DOCS`.
const VIEWS: &str = "
CREATE TEMP VIEW history AS
SELECT l.id, l.started_at,
       datetime(l.started_at, 'unixepoch', 'localtime') AS started_local,
       CAST(strftime('%H', l.started_at, 'unixepoch', 'localtime') AS INTEGER) AS hour,
       CAST(strftime('%w', l.started_at, 'unixepoch', 'localtime') AS INTEGER) AS weekday,
       l.ms AS played_ms,
       l.ms >= MIN(30000, COALESCE(l.duration_ms, 60000) / 2) AS play,
       l.ended, l.position_ms, l.duration_ms, l.title,
       COALESCE(l.artists, '[]') AS artists, l.album, l.source, l.shuffled, l.output, l.video_id,
       EXISTS (SELECT 1 FROM main.tracks t WHERE t.video_id = l.video_id) AS in_library
FROM main.listens l;
CREATE TEMP VIEW library AS
SELECT video_id, title, artists, album, album_artist, year, CAST(duration AS INTEGER) AS duration_s,
       datetime(added_at, 'unixepoch', 'localtime') AS added_local
FROM main.tracks;
CREATE TEMP VIEW playlist_songs AS
SELECT p.name AS playlist, pt.position, t.title, t.artists, t.album, t.video_id
FROM main.playlists p JOIN main.playlist_tracks pt ON pt.playlist_id = p.id JOIN main.tracks t ON t.id = pt.track_id;
";

/// The views as the model is told about them (with the SQL tool).
pub const VIEW_DOCS: &str = "\
history: one row per listen, oldest first by id.
  started_at (unix s), started_local ('YYYY-MM-DD HH:MM:SS', local time), hour (0-23), weekday (0 = Sunday),
  played_ms (time actually played), play (1 if it counts as a play: 30 s, or half a song shorter than a minute),
  ended ('finished', 'skipped' (went to another song), 'replaced' (another list was started),
         'stopped' (the player stopped), or NULL for listens logged before this was recorded),
  position_ms (where in the song it ended), duration_ms, title, artists (JSON array: use json_each),
  album, source (what it was played from: 'playlist: Gym', 'album: Mordechai' (or 'single: …', 'ep: …'),
  'radio: Maria También', 'artist: Khruangbin', 'search', 'home', 'library', 'downloads', 'stats', 'insights',
  'queue' (queued by hand), 'resumed' (the queue restored after a restart), 'car: playlists' (picked in the
  car, from that list); NULL when not recorded), shuffled (0/1, NULL when not recorded),
  output ('speaker', 'wired', 'bluetooth: <device name>', 'usb', 'car', or NULL when not recorded),
  video_id (YouTube id), in_library (1 if the song is downloaded now).
library: the downloaded songs: video_id, title, artists (JSON array), album, album_artist, year, duration_s,
  added_local.
playlist_songs: playlist (name), position, title, artists (JSON array), album, video_id.";

/// What a query returned.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// More rows were left out.
    pub truncated: bool,
}

/// How much history there is, for Settings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HistoryInfo {
    pub listens: i64,
    /// Unix seconds.
    pub first: Option<i64>,
}

/// Rows a query returns at most.
const MAX_ROWS: usize = 200;

impl Library {
    pub fn history_info(&self) -> Result<HistoryInfo> {
        Ok(self.db().query_row("SELECT COUNT(*), MIN(started_at) FROM listens", [], |r| {
            Ok(HistoryInfo { listens: r.get(0)?, first: r.get(1)? })
        })?)
    }

    /// How far local time is ahead of UTC now, in seconds.
    pub fn utc_offset(&self) -> Result<i64> {
        Ok(self.db().query_row(
            "SELECT CAST(strftime('%s', 'now', 'localtime') AS INTEGER) - CAST(strftime('%s', 'now') AS INTEGER)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Runs one read-only SELECT over the history views.
    pub fn query_history(&self, sql: &str) -> Result<QueryResult> {
        let conn = self.reader()?;
        query(&conn, sql, MAX_ROWS)
    }

    /// An overview of the listening history, for a model to start from: JSON
    /// tables of totals, tops, times, skips, sources and the latest listens.
    pub fn history_summary(&self, detailed: bool) -> Result<Value> {
        let conn = self.reader()?;
        let mut out = Map::new();
        let detail = if detailed { DETAIL } else { &[] };
        for (name, sql) in SUMMARY.iter().chain(detail) {
            let result = query(&conn, sql, MAX_ROWS)?;
            out.insert((*name).into(), json!({ "columns": result.columns, "rows": result.rows }));
        }
        Ok(Value::Object(out))
    }

    /// Writes every listen to `path` as a JSON array of objects.
    pub fn export_history(&self, path: &Path) -> Result<usize> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare("SELECT * FROM history ORDER BY id")?;
        let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
        let mut rows = stmt.query([])?;
        let mut listens = Vec::new();
        while let Some(row) = rows.next()? {
            let mut object = Map::new();
            for (i, name) in columns.iter().enumerate() {
                object.insert(name.clone(), value(row.get_ref(i)?));
            }
            listens.push(Value::Object(object));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(&listens)?)?;
        Ok(listens.len())
    }

    /// A read-only connection with the views.
    fn reader(&self) -> Result<Connection> {
        let conn = match &self.0.path {
            Some(path) => Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
            )?,
            // Tests: a copy of the in-memory database.
            None => {
                let mut copy = Connection::open_in_memory()?;
                let db = self.db();
                rusqlite::backup::Backup::new(&db, &mut copy)?.run_to_completion(256, std::time::Duration::ZERO, None)?;
                drop(db);
                copy
            }
        };
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(VIEWS)?;
        conn.pragma_update(None, "query_only", true)?;
        Ok(conn)
    }
}

/// Runs `sql`, which must be one SELECT (or WITH ... SELECT), for up to `max` rows.
fn query(conn: &Connection, sql: &str, max: usize) -> Result<QueryResult> {
    let sql = sql.trim().trim_end_matches(';').trim();
    let head = sql.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
    if head != "select" && head != "with" {
        return Err(Error::Invalid("only SELECT queries can be run".into()));
    }
    let mut stmt = conn.prepare(sql)?;
    if !stmt.readonly() {
        return Err(Error::Invalid("only SELECT queries can be run".into()));
    }
    let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if out.len() == max {
            truncated = true;
            break;
        }
        out.push((0..columns.len()).map(|i| row.get_ref(i).map(value)).collect::<rusqlite::Result<_>>()?);
    }
    Ok(QueryResult { columns, rows: out, truncated })
}

fn value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!((f * 100.0).round() / 100.0),
        ValueRef::Text(t) => {
            let text = String::from_utf8_lossy(t);
            // JSON columns (artists) as JSON.
            if text.starts_with('[') {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    return v;
                }
            }
            Value::String(text.into_owned())
        }
        ValueRef::Blob(b) => json!(format!("<{} bytes>", b.len())),
    }
}

/// The summary's tables, each a query over the views.
const SUMMARY: &[(&str, &str)] = &[
    (
        "today",
        "SELECT datetime('now', 'localtime') AS now_local,
                (SELECT MIN(started_local) FROM history) AS first_listen,
                (SELECT COUNT(*) FROM history) AS listens,
                (SELECT COUNT(*) FROM history WHERE ended IS NOT NULL) AS listens_with_details",
    ),
    (
        "totals",
        "SELECT w.name AS period, ROUND(COALESCE(SUM(h.played_ms), 0) / 3600000.0, 1) AS hours,
                COALESCE(SUM(h.play), 0) AS plays, COUNT(DISTINCT h.video_id) AS songs
         FROM (SELECT 'last 7 days' AS name, 7 AS days, 1 AS o UNION ALL SELECT 'last 30 days', 30, 2
               UNION ALL SELECT 'last 90 days', 90, 3 UNION ALL SELECT 'last 365 days', 365, 4
               UNION ALL SELECT 'all time', 100000, 5) w
         LEFT JOIN history h ON h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - w.days * 86400
         GROUP BY w.name ORDER BY w.o",
    ),
    (
        "top_artists_90_days",
        "SELECT a.value AS artist, SUM(h.play) AS plays, ROUND(SUM(h.played_ms) / 3600000.0, 1) AS hours,
                SUM(h.ended = 'skipped') AS skipped, COUNT(DISTINCT h.video_id) AS songs
         FROM history h, json_each(h.artists) a
         WHERE h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY a.value ORDER BY plays DESC, hours DESC LIMIT 25",
    ),
    (
        "top_artists_all_time",
        "SELECT a.value AS artist, SUM(h.play) AS plays, ROUND(SUM(h.played_ms) / 3600000.0, 1) AS hours,
                MIN(date(h.started_local)) AS first_played, MAX(date(h.started_local)) AS last_played
         FROM history h, json_each(h.artists) a
         GROUP BY a.value ORDER BY plays DESC, hours DESC LIMIT 25",
    ),
    (
        "top_songs_90_days",
        "SELECT title, artists, album, SUM(play) AS plays, SUM(ended = 'skipped') AS skipped,
                MAX(in_library) AS in_library
         FROM history WHERE started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY COALESCE(video_id, title) ORDER BY plays DESC LIMIT 25",
    ),
    (
        "top_songs_all_time",
        "SELECT title, artists, SUM(play) AS plays, MIN(date(started_local)) AS first_played,
                MAX(date(started_local)) AS last_played
         FROM history GROUP BY COALESCE(video_id, title) ORDER BY plays DESC LIMIT 25",
    ),
    (
        "top_albums_90_days",
        "SELECT album, artists, SUM(play) AS plays FROM history
         WHERE album IS NOT NULL AND album != ''
           AND started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY album ORDER BY plays DESC LIMIT 10",
    ),
    (
        "hours_by_hour_of_day_90_days",
        "SELECT hour, ROUND(SUM(played_ms) / 3600000.0, 1) AS hours FROM history
         WHERE started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400 GROUP BY hour ORDER BY hour",
    ),
    (
        "hours_by_weekday_90_days",
        "SELECT weekday, ROUND(SUM(played_ms) / 3600000.0, 1) AS hours FROM history
         WHERE started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400 GROUP BY weekday ORDER BY weekday",
    ),
    (
        "hours_by_month",
        "SELECT strftime('%Y-%m', started_local) AS month, ROUND(SUM(played_ms) / 3600000.0, 1) AS hours,
                SUM(play) AS plays, COUNT(DISTINCT video_id) AS songs
         FROM history GROUP BY month ORDER BY month DESC LIMIT 24",
    ),
    (
        "how_listens_ended_90_days",
        "SELECT ended, COUNT(*) AS listens,
                SUM(position_ms < 30000) AS in_first_30_s,
                ROUND(AVG(CASE WHEN duration_ms > 0 THEN 1.0 * position_ms / duration_ms END), 2) AS avg_share_heard
         FROM history WHERE ended IS NOT NULL
           AND started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY ended ORDER BY listens DESC",
    ),
    (
        "sources_90_days",
        "SELECT source, COUNT(*) AS listens, SUM(play) AS plays, SUM(ended = 'skipped') AS skipped,
                SUM(shuffled) AS shuffled
         FROM history WHERE source IS NOT NULL
           AND started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY source ORDER BY listens DESC LIMIT 25",
    ),
    (
        "outputs_90_days",
        "SELECT output, COUNT(*) AS listens, ROUND(SUM(played_ms) / 3600000.0, 1) AS hours FROM history
         WHERE output IS NOT NULL AND started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY output ORDER BY listens DESC",
    ),
    (
        "new_artists_by_month",
        "SELECT strftime('%Y-%m', first) AS month, COUNT(*) AS new_artists, group_concat(artist, ', ') AS some
         FROM (SELECT a.value AS artist, MIN(h.started_local) AS first FROM history h, json_each(h.artists) a
               WHERE h.play GROUP BY a.value)
         GROUP BY month ORDER BY month DESC LIMIT 12",
    ),
    (
        "faded_artists",
        "SELECT a.value AS artist, SUM(h.play) AS plays, MAX(date(h.started_local)) AS last_played
         FROM history h, json_each(h.artists) a GROUP BY a.value
         HAVING SUM(h.play) >= 10 AND MAX(h.started_at) < CAST(strftime('%s', 'now') AS INTEGER) - 60 * 86400
         ORDER BY plays DESC LIMIT 15",
    ),
    (
        "library",
        "SELECT (SELECT COUNT(*) FROM library) AS songs,
                (SELECT COUNT(DISTINCT a.value) FROM library, json_each(library.artists) a) AS artists,
                (SELECT COUNT(DISTINCT album) FROM library WHERE album IS NOT NULL) AS albums,
                (SELECT group_concat(playlist || ' (' || n || ')', ', ')
                 FROM (SELECT playlist, COUNT(*) AS n FROM playlist_songs GROUP BY playlist)) AS playlists",
    ),
    (
        "latest_listens",
        "SELECT started_local, title, artists, ROUND(played_ms / 1000.0) AS played_s, ended, source, output
         FROM history ORDER BY id DESC LIMIT 40",
    ),
];

/// More tables, for a model that can't run its own queries (a batched report).
const DETAIL: &[(&str, &str)] = &[
    (
        "most_skipped_songs_90_days",
        "SELECT title, artists, COUNT(*) AS listens, SUM(ended = 'skipped') AS skipped,
                ROUND(AVG(CASE WHEN ended = 'skipped' THEN position_ms / 1000.0 END)) AS avg_skip_at_s
         FROM history WHERE ended IS NOT NULL
           AND started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY COALESCE(video_id, title) HAVING skipped >= 2 ORDER BY skipped DESC LIMIT 20",
    ),
    (
        "skip_rate_by_hour_band_90_days",
        "SELECT CASE WHEN hour < 6 THEN '0-6' WHEN hour < 12 THEN '6-12' WHEN hour < 18 THEN '12-18' ELSE '18-24' END
                AS hours, COUNT(*) AS listens, ROUND(AVG(ended = 'skipped'), 2) AS skip_rate
         FROM history WHERE ended IS NOT NULL
           AND started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
         GROUP BY 1 ORDER BY 1",
    ),
    (
        "top_artists_by_hour_band_90_days",
        "SELECT band, artist, plays FROM (
           SELECT CASE WHEN h.hour < 6 THEN '0-6' WHEN h.hour < 12 THEN '6-12' WHEN h.hour < 18 THEN '12-18'
                  ELSE '18-24' END AS band, a.value AS artist, SUM(h.play) AS plays,
                  ROW_NUMBER() OVER (PARTITION BY CASE WHEN h.hour < 6 THEN 0 WHEN h.hour < 12 THEN 1
                    WHEN h.hour < 18 THEN 2 ELSE 3 END ORDER BY SUM(h.play) DESC) AS n
           FROM history h, json_each(h.artists) a
           WHERE h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
           GROUP BY band, a.value)
         WHERE n <= 6 AND plays > 0 ORDER BY band, plays DESC",
    ),
    (
        "top_artists_by_output_90_days",
        "SELECT output, artist, plays FROM (
           SELECT h.output, a.value AS artist, SUM(h.play) AS plays,
                  ROW_NUMBER() OVER (PARTITION BY h.output ORDER BY SUM(h.play) DESC) AS n
           FROM history h, json_each(h.artists) a
           WHERE h.output IS NOT NULL AND h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
           GROUP BY h.output, a.value)
         WHERE n <= 5 AND plays > 0 ORDER BY output, plays DESC",
    ),
    (
        "weekend_vs_weekday_top_artists_90_days",
        "SELECT part, artist, plays FROM (
           SELECT CASE WHEN h.weekday IN (0, 6) THEN 'weekend' ELSE 'weekday' END AS part, a.value AS artist,
                  SUM(h.play) AS plays,
                  ROW_NUMBER() OVER (PARTITION BY h.weekday IN (0, 6) ORDER BY SUM(h.play) DESC) AS n
           FROM history h, json_each(h.artists) a
           WHERE h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400
           GROUP BY part, a.value)
         WHERE n <= 8 AND plays > 0 ORDER BY part, plays DESC",
    ),
    (
        "rising_artists",
        "SELECT a.value AS artist,
                SUM(h.play * (h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 30 * 86400)) AS plays_last_30_days,
                SUM(h.play * (h.started_at < CAST(strftime('%s', 'now') AS INTEGER) - 30 * 86400
                     AND h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 120 * 86400)) AS plays_90_days_before
         FROM history h, json_each(h.artists) a
         WHERE h.started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 120 * 86400
         GROUP BY a.value HAVING plays_last_30_days >= 5
         ORDER BY 1.0 * plays_last_30_days / (plays_90_days_before + 3) DESC LIMIT 15",
    ),
    (
        "songs_on_repeat",
        "SELECT title, artists, date(started_local) AS day, SUM(play) AS plays FROM history
         GROUP BY COALESCE(video_id, title), day HAVING plays >= 4 ORDER BY plays DESC, day DESC LIMIT 15",
    ),
    (
        "listening_days",
        "SELECT COUNT(DISTINCT date(started_local)) AS days_with_listening_last_90,
                ROUND(SUM(played_ms) / 3600000.0 / MAX(COUNT(DISTINCT date(started_local)), 1), 1) AS hours_per_listening_day
         FROM history WHERE started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400",
    ),
    (
        "longest_sessions_90_days",
        "WITH s AS (SELECT *, SUM(gap) OVER (ORDER BY started_at) AS session FROM (
                      SELECT started_at, started_local, played_ms, artists,
                             (started_at - LAG(started_at) OVER (ORDER BY started_at) > 1800) AS gap
                      FROM history WHERE started_at >= CAST(strftime('%s', 'now') AS INTEGER) - 90 * 86400))
         SELECT MIN(started_local) AS started, ROUND(SUM(played_ms) / 3600000.0, 1) AS hours, COUNT(*) AS listens
         FROM s GROUP BY session ORDER BY hours DESC LIMIT 10",
    ),
    (
        "library_songs_never_played",
        "SELECT COUNT(*) AS songs,
                (SELECT group_concat(title || ' - ' || artists, '; ') FROM
                   (SELECT l.title, l.artists FROM library l
                    WHERE NOT EXISTS (SELECT 1 FROM history h WHERE h.video_id = l.video_id)
                    ORDER BY random() LIMIT 15)) AS some
         FROM library l WHERE NOT EXISTS (SELECT 1 FROM history h WHERE h.video_id = l.video_id)",
    ),
];
