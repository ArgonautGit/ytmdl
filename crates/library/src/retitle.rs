//! Tidying the titles of songs already in the library (see
//! [`ytmdl_core::titles`]): songs downloaded from videos before ytmdl tidied
//! their titles.

use rusqlite::params;
use ytmdl_core::titles;

use crate::{Error, Library, Result, TRACK_COLUMNS, Track, file_stamp, track_from_row};

/// A song whose title or artists [`titles::tidy`] changes.
#[derive(Debug, Clone, PartialEq)]
pub struct Retitle {
    pub track: Track,
    pub title: String,
    pub artists: Vec<String>,
}

impl Library {
    /// The songs whose titles aren't tidy, most recently added first.
    pub fn untidy(&self) -> Result<Vec<Retitle>> {
        let tracks = self.tracks()?;
        Ok(tracks
            .into_iter()
            .filter_map(|track| {
                let (title, artists) = titles::tidy(&track.title, &track.artists);
                (title != track.title || artists != track.artists).then_some(Retitle { track, title, artists })
            })
            .collect())
    }

    /// Retitles a song: in its file's tags, in the file's name and folder
    /// (unless `rename` is false, for a file the player may have open), and
    /// in the index. Blocking. Returns the song as it is now.
    pub fn retitle(&self, r: &Retitle, rename: bool) -> Result<Track> {
        let t = &r.track;
        ytmdl_core::tag::retitle(&t.path, &r.title, &r.artists).map_err(|e| Error::Invalid(e.to_string()))?;
        let (old_artist, new_artist) = (t.artists.first().map(String::as_str), r.artists.first().map(String::as_str));
        let mut path = t.path.clone();
        if rename
            && let Some(to) = titles::retitled_path(&t.path, &t.video_id, (&t.title, old_artist), (&r.title, new_artist))
        {
            match titles::relocate(&t.path, &to) {
                Ok(()) => path = to,
                Err(e) => tracing::warn!(target: "ytmdl", "could not rename {} to {}: {e}", t.path.display(), to.display()),
            }
        }
        // An album artist that was the first artist follows it, as in the tags.
        let album_artist = match (old_artist, new_artist) {
            (Some(old), Some(new)) if old == t.album_artist => new.to_owned(),
            _ => t.album_artist.clone(),
        };
        let (size, mtime) = file_stamp(&path)?;
        let sql = format!(
            "UPDATE tracks SET title = ?2, artists = ?3, album_artist = ?4, path = ?5, file_size = ?6, file_mtime = ?7
             WHERE id = ?1 RETURNING {TRACK_COLUMNS}"
        );
        let params = params![
            t.id,
            r.title,
            serde_json::to_string(&r.artists)?,
            album_artist,
            path.to_string_lossy(),
            size as i64,
            mtime
        ];
        Ok(self.db().query_row(&sql, params, track_from_row)?)
    }
}
