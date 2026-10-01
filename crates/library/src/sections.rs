//! Saved A-B sections: named loops over part of a song ("chorus", "solo"). They
//! belong to the video, not the file, so they survive a song being deleted and
//! downloaded again.
//!
//! A section can also be skipped: the player leaves it out whenever the song
//! plays (a long intro, a skit). The player's process reads those itself (see
//! app/android/PlaybackService.kt), so they hold in the car and with the app
//! closed.

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{Error, Library, Result, now};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: i64,
    pub video_id: String,
    pub name: String,
    /// Where the loop starts and ends, in ms.
    pub a_ms: i64,
    pub b_ms: i64,
    /// Left out when the song plays.
    #[serde(default)]
    pub skip: bool,
}

impl Library {
    /// A song's sections, in the order they come in the song.
    pub fn sections(&self, video_id: &str) -> Result<Vec<Section>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT id, video_id, name, a_ms, b_ms, skip FROM sections WHERE video_id = ?1 ORDER BY a_ms, b_ms, id",
        )?;
        let rows = stmt.query_map([video_id], |r| {
            Ok(Section {
                id: r.get(0)?,
                video_id: r.get(1)?,
                name: r.get(2)?,
                a_ms: r.get(3)?,
                b_ms: r.get(4)?,
                skip: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_section(&self, video_id: &str, name: &str, a_ms: i64, b_ms: i64) -> Result<Section> {
        let name = name.trim();
        if name.is_empty() || a_ms < 0 || b_ms <= a_ms {
            return Err(Error::Invalid(format!("not a section: {name:?} {a_ms}..{b_ms}")));
        }
        let db = self.db();
        db.execute(
            "INSERT INTO sections (video_id, name, a_ms, b_ms, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![video_id, name, a_ms, b_ms, now()],
        )?;
        Ok(Section { id: db.last_insert_rowid(), video_id: video_id.into(), name: name.into(), a_ms, b_ms, skip: false })
    }

    pub fn rename_section(&self, id: i64, name: &str) -> Result<()> {
        self.db().execute("UPDATE sections SET name = ?2 WHERE id = ?1", params![id, name.trim()])?;
        Ok(())
    }

    /// Whether the player leaves the section out.
    pub fn set_section_skip(&self, id: i64, skip: bool) -> Result<()> {
        self.db().execute("UPDATE sections SET skip = ?2 WHERE id = ?1", params![id, skip])?;
        Ok(())
    }

    pub fn delete_section(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM sections WHERE id = ?1", [id])?;
        Ok(())
    }
}
