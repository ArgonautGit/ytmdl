//! Songs in the library twice, from different uploads (a song and its music
//! video, say): tracks with the same [`song_key`].

use std::collections::{HashMap, HashSet};

use rusqlite::params;
use ytmdl_core::titles::song_key;

use crate::{Library, Result, Track};

/// Songs that look like one song.
#[derive(Debug, Clone, PartialEq)]
pub struct Duplicates {
    /// Their [`song_key`].
    pub key: String,
    /// The first downloaded first.
    pub tracks: Vec<Track>,
}

impl Library {
    /// The songs in the library more than once, the group with the latest
    /// download first. Groups kept with [`Library::keep_duplicates`] are left
    /// out, until another upload of the song joins them.
    pub fn duplicates(&self) -> Result<Vec<Duplicates>> {
        let kept = self.kept_duplicates()?;
        let mut groups: HashMap<String, Vec<Track>> = HashMap::new();
        for track in self.tracks()?.into_iter().rev() {
            if let Some(key) = song_key(&track.title, &track.artists) {
                groups.entry(key).or_default().push(track);
            }
        }
        let mut found: Vec<Duplicates> = groups
            .into_iter()
            .filter(|(key, tracks)| {
                tracks.len() > 1 && !kept.get(key).is_some_and(|ids| tracks.iter().all(|t| ids.contains(&t.video_id)))
            })
            .map(|(key, tracks)| Duplicates { key, tracks })
            .collect();
        found.sort_by_key(|d| std::cmp::Reverse(d.tracks.last().map(|t| (t.added_at, t.id))));
        Ok(found)
    }

    /// Keeps a group's songs: [`Library::duplicates`] stops listing them.
    pub fn keep_duplicates(&self, group: &Duplicates) -> Result<()> {
        let ids: Vec<&str> = group.tracks.iter().map(|t| t.video_id.as_str()).collect();
        self.db().execute(
            "INSERT INTO kept_duplicates (key, video_ids) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET video_ids = excluded.video_ids",
            params![group.key, serde_json::to_string(&ids)?],
        )?;
        Ok(())
    }

    fn kept_duplicates(&self) -> Result<HashMap<String, HashSet<String>>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT key, video_ids FROM kept_duplicates")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut kept = HashMap::new();
        for row in rows {
            let (key, ids) = row?;
            kept.insert(key, serde_json::from_str(&ids)?);
        }
        Ok(kept)
    }

    /// The songs in the library by [`song_key`], to tell when a search result
    /// is another upload of one of them. The first downloaded, of several.
    pub fn song_keys(&self) -> Result<HashMap<String, Track>> {
        Ok(self.tracks()?.into_iter().filter_map(|t| Some((song_key(&t.title, &t.artists)?, t))).collect())
    }
}
