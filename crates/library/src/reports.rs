//! Listening insights over time: every report kept (the app's JSON, see
//! app/src/insights.rs), and a few habits measured month by month from the
//! history, to see how they change.

use rusqlite::params;
use serde::Serialize;

use crate::{Library, Result};

/// One month of the history, measured. Shares are 0 to 1, `None` when there
/// is nothing to count them from (such as skips before they were logged).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HabitMonth {
    /// "2026-10", local time.
    pub month: String,
    /// Over the days of the month the history covers (to today, from the
    /// first listen).
    pub minutes_a_day: f64,
    pub plays: i64,
    /// Of the listens whose end was logged, those skipped.
    pub skipped: Option<f64>,
    /// Of the plays, those of songs first played that month; `None` in the
    /// history's first month, when every song is.
    pub new: Option<f64>,
    /// Of the plays, those of the month's 10 most played songs.
    pub top_songs: Option<f64>,
    /// Of the plays whose output was logged, those in the car.
    pub car: Option<f64>,
    /// Of the plays, those after midnight (before 5).
    pub late_night: Option<f64>,
}

/// Every month from the first listen's to this one, empty ones included.
const HABIT_MONTHS: &str = "
WITH RECURSIVE
  months(m) AS (
    SELECT date(MIN(started_local), 'start of month') FROM history HAVING COUNT(*) > 0
    UNION ALL
    SELECT date(m, '+1 month') FROM months WHERE date(m, '+1 month') <= date('now', 'localtime', 'start of month')
  ),
  h AS (
    SELECT h.*, date(h.started_local, 'start of month') AS m, COALESCE(h.video_id, h.title, '') AS k FROM history h
  ),
  firsts AS (SELECT k, MIN(m) AS first FROM h GROUP BY k),
  top AS (
    SELECT m, SUM(p) AS plays FROM (
      SELECT m, SUM(play) AS p, ROW_NUMBER() OVER (PARTITION BY m ORDER BY SUM(play) DESC) AS n FROM h GROUP BY m, k
    ) WHERE n <= 10 GROUP BY m
  )
SELECT months.m,
       julianday(MIN(date(months.m, '+1 month'), date('now', 'localtime', '+1 day')))
         - julianday(MAX(months.m, (SELECT date(MIN(started_local)) FROM history))) AS days,
       COALESCE(SUM(h.played_ms), 0), COALESCE(SUM(h.play), 0),
       COALESCE(SUM(h.ended = 'skipped'), 0), COUNT(h.ended),
       COALESCE(SUM(h.play * (f.first = h.m)), 0),
       COALESCE((SELECT plays FROM top WHERE top.m = months.m), 0),
       COALESCE(SUM(h.play * (h.output = 'car')), 0), COALESCE(SUM(h.play * (h.output IS NOT NULL)), 0),
       COALESCE(SUM(h.play * (h.hour < 5)), 0)
FROM months LEFT JOIN h ON h.m = months.m LEFT JOIN firsts f ON f.k = h.k
GROUP BY months.m ORDER BY months.m";

impl Library {
    /// Keeps a report; its id.
    pub fn add_report(&self, made_at: i64, report: &str) -> Result<i64> {
        let db = self.db();
        db.execute("INSERT INTO insight_reports (made_at, report) VALUES (?1, ?2)", params![made_at, report])?;
        Ok(db.last_insert_rowid())
    }

    /// Every report, newest first: its id and JSON.
    pub fn reports(&self) -> Result<Vec<(i64, String)>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, report FROM insight_reports ORDER BY made_at DESC, id DESC")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The habits, month by month, oldest first.
    pub fn habit_months(&self) -> Result<Vec<HabitMonth>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(HABIT_MONTHS)?;
        let share = |part: i64, whole: i64| (whole > 0).then(|| part as f64 / whole as f64);
        let rows = stmt.query_map([], |r| {
            let days: f64 = r.get(1)?;
            let (ms, plays): (i64, i64) = (r.get(2)?, r.get(3)?);
            Ok(HabitMonth {
                month: r.get::<_, String>(0)?[..7].to_string(),
                minutes_a_day: ms as f64 / 60_000.0 / days.max(1.0),
                plays,
                skipped: share(r.get(4)?, r.get(5)?),
                new: share(r.get(6)?, plays),
                top_songs: share(r.get(7)?, plays),
                car: share(r.get(8)?, r.get(9)?),
                late_night: share(r.get(10)?, plays),
            })
        })?;
        let mut months: Vec<HabitMonth> = rows.collect::<rusqlite::Result<_>>()?;
        if let Some(first) = months.first_mut() {
            first.new = None;
        }
        Ok(months)
    }
}
