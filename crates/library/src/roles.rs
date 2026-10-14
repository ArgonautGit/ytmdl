//! The two playlists with a role. The secondary holds current favourites, the
//! songs to hear more often, and new songs join it as they come into the
//! library. The primary holds the songs worth keeping, for when the secondary's
//! wear out. They are separate: a song can be in either, both or neither.
//!
//! The roles are settings naming a playlist each, so a playlist deleted drops
//! its role.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use crate::{Library, Result, Track};

const PRIMARY: &str = "playlist.primary";
const SECONDARY: &str = "playlist.secondary";
/// New songs join the secondary ("0" is off).
const NEW_TO_SECONDARY: &str = "playlist.secondary.new";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Primary,
    Secondary,
}

impl Role {
    fn key(self) -> &'static str {
        match self {
            Role::Primary => PRIMARY,
            Role::Secondary => SECONDARY,
        }
    }
}

/// The playlists with a role, when they have one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Roles {
    pub primary: Option<i64>,
    pub secondary: Option<i64>,
}

impl Roles {
    pub fn get(&self, role: Role) -> Option<i64> {
        match role {
            Role::Primary => self.primary,
            Role::Secondary => self.secondary,
        }
    }

    /// The role of playlist `id`, if it has one.
    pub fn of(&self, id: i64) -> Option<Role> {
        if self.primary == Some(id) {
            Some(Role::Primary)
        } else if self.secondary == Some(id) {
            Some(Role::Secondary)
        } else {
            None
        }
    }
}

impl Library {
    /// The playlists with a role, leaving out deleted ones.
    pub fn roles(&self) -> Result<Roles> {
        let db = self.db();
        let get = |key: &str| -> rusqlite::Result<Option<i64>> {
            db.query_row(
                "SELECT p.id FROM settings s JOIN playlists p ON p.id = CAST(s.value AS INTEGER) WHERE s.key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()
        };
        Ok(Roles { primary: get(PRIMARY)?, secondary: get(SECONDARY)? })
    }

    /// Gives playlist `id` the role (None: no playlist has it). A playlist has
    /// one role at most, so it loses the other one.
    pub fn set_role(&self, role: Role, id: Option<i64>) -> Result<()> {
        let db = self.db();
        match id {
            Some(id) => {
                let other = match role {
                    Role::Primary => SECONDARY,
                    Role::Secondary => PRIMARY,
                };
                db.execute("DELETE FROM settings WHERE key = ?1 AND value = ?2", params![other, id.to_string()])?;
                db.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![role.key(), id.to_string()],
                )?;
            }
            None => {
                db.execute("DELETE FROM settings WHERE key = ?1", [role.key()])?;
            }
        }
        Ok(())
    }

    /// Whether new songs join the secondary playlist (on unless turned off).
    pub fn new_to_secondary(&self) -> Result<bool> {
        Ok(self.setting(NEW_TO_SECONDARY)?.as_deref() != Some("0"))
    }

    pub fn set_new_to_secondary(&self, on: bool) -> Result<()> {
        self.set_setting(NEW_TO_SECONDARY, if on { "1" } else { "0" })
    }

    /// The playlists each track is in, by track id; each list in the order the
    /// playlists were made.
    pub fn memberships(&self) -> Result<HashMap<i64, Vec<i64>>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT DISTINCT pt.track_id, pt.playlist_id FROM playlist_tracks pt
             JOIN playlists p ON p.id = pt.playlist_id ORDER BY p.created_at, p.id",
        )?;
        let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (track, playlist) = row?;
            out.entry(track).or_default().push(playlist);
        }
        Ok(out)
    }

    /// Takes a track out of a playlist, as [`Library::remove_from_playlist`]
    /// takes out an entry.
    pub fn remove_track_from_playlist(&self, id: i64, track: i64) -> Result<()> {
        let entries: Vec<i64> = {
            let db = self.db();
            let mut stmt = db.prepare("SELECT id FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2")?;
            stmt.query_map(params![id, track], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        for entry in entries {
            self.remove_from_playlist(entry)?;
        }
        Ok(())
    }

    /// Puts a song that just came into the library (`new`: it wasn't in it
    /// before) in the secondary playlist, unless that is turned off or the song
    /// came with a synced playlist.
    pub(crate) fn join_secondary(&self, track: &Track, new: bool) -> Result<()> {
        if !new || !self.new_to_secondary()? {
            return Ok(());
        }
        let Some(secondary) = self.roles()?.secondary else { return Ok(()) };
        let synced: bool = self.db().query_row(
            "SELECT EXISTS (SELECT 1 FROM playlist_remote WHERE video_id = ?1 AND NOT removed)",
            [&track.video_id],
            |r| r.get(0),
        )?;
        if !synced {
            self.add_to_playlist(secondary, &[track.id])?;
        }
        Ok(())
    }

    /// Whether the library has a song of the video.
    pub(crate) fn has_video(&self, video_id: &str) -> Result<bool> {
        Ok(self.db().query_row("SELECT EXISTS (SELECT 1 FROM tracks WHERE video_id = ?1)", [video_id], |r| r.get(0))?)
    }
}
