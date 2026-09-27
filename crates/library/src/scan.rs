//! Rebuilding the index from the music folders: files ytmdl downloaded are named
//! `<title> [<video id>].<ext>` and tagged with their source URL.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lofty::picture::PictureType;
use lofty::prelude::*;
use lofty::tag::ItemKey;

use crate::{Library, NewTrack, Result, file_stamp};

/// Folder depth below a scanned root (`<Artist>/<Album>/<file>` needs 3).
const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

impl ScanReport {
    pub fn changed(&self) -> bool {
        self.added + self.updated + self.removed > 0
    }
}

struct Known {
    id: i64,
    video_id: String,
    stamp: (u64, i64),
}

impl Library {
    /// Indexes new and changed ytmdl files under `dirs`, and forgets tracks under
    /// `dirs` whose files are gone. Folders that can't be listed (no storage
    /// access) are left alone. Blocking.
    pub fn scan(&self, dirs: &[PathBuf]) -> Result<ScanReport> {
        let known: HashMap<PathBuf, Known> = {
            let db = self.db();
            let mut stmt = db.prepare("SELECT id, video_id, path, file_size, file_mtime FROM tracks")?;
            let rows = stmt.query_map([], |r| {
                let path: String = r.get(2)?;
                let stamp = (r.get::<_, i64>(3)? as u64, r.get(4)?);
                Ok((PathBuf::from(path), Known { id: r.get(0)?, video_id: r.get(1)?, stamp }))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let path_of: HashMap<&str, &Path> = known.iter().map(|(p, k)| (k.video_id.as_str(), p.as_path())).collect();

        let mut report = ScanReport::default();
        let mut seen = HashSet::new();
        let mut listed = Vec::new();
        for dir in dirs {
            let mut files = Vec::new();
            if walk(dir, 0, &mut files).is_err() {
                continue;
            }
            listed.push(dir);
            for (path, video_id) in files {
                seen.insert(path.clone());
                let stamp = file_stamp(&path)?;
                if known.get(&path).is_some_and(|k| k.stamp == stamp) {
                    continue;
                }
                // The same video saved twice: keep the copy already indexed.
                if path_of.get(video_id.as_str()).is_some_and(|p| *p != path && p.exists()) {
                    continue;
                }
                let Some(mut track) = read_file(&path, video_id) else { continue };
                track.art = read_cover(&path).and_then(|data| self.0.art.store(&data));
                (track.file_size, track.file_mtime) = stamp;
                self.upsert(&track)?;
                if known.contains_key(&path) {
                    report.updated += 1;
                } else {
                    report.added += 1;
                }
            }
        }

        for (path, k) in &known {
            if !seen.contains(path) && listed.iter().any(|d| path.starts_with(d)) && !path.exists() {
                self.db().execute("DELETE FROM tracks WHERE id = ?1", [k.id])?;
                report.removed += 1;
            }
        }
        self.add_artists_tags(&listed)?;
        if report.changed() {
            tracing::info!(target: "ytmdl", "library scan: {report:?}");
        }
        Ok(report)
    }
}

impl Library {
    /// Files tagged before ytmdl wrote each artist separately only have the
    /// artists joined with ", ", which a scan would split inside a name like
    /// "Tyler, The Creator". The index still knows the real names, so write
    /// them into those files before the index is ever rebuilt from them.
    fn add_artists_tags(&self, dirs: &[&PathBuf]) -> Result<()> {
        let tracks: Vec<(i64, PathBuf, String)> = {
            let db = self.db();
            let mut stmt = db.prepare(
                "SELECT id, path, artists FROM tracks t
                 WHERE EXISTS (SELECT 1 FROM json_each(t.artists) WHERE value LIKE '%, %')",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?), r.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (id, path, artists) in tracks {
            if !dirs.iter().any(|d| path.starts_with(d)) {
                continue;
            }
            let artists: Vec<String> = serde_json::from_str(&artists)?;
            match ytmdl_core::tag::add_artists(&path, &artists) {
                Ok(true) => {
                    // Keep the scan from taking the new tag for a changed file.
                    let (size, mtime) = file_stamp(&path)?;
                    self.db().execute(
                        "UPDATE tracks SET file_size = ?2, file_mtime = ?3 WHERE id = ?1",
                        rusqlite::params![id, size as i64, mtime],
                    )?;
                    tracing::info!(target: "ytmdl", "wrote the artists of {}", path.display());
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(target: "ytmdl", "writing the artists of {}: {e}", path.display()),
            }
        }
        Ok(())
    }
}

/// Collects `(path, video id)` of ytmdl's audio files below `dir`.
fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, String)>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if !hidden && depth < MAX_DEPTH {
                let _ = walk(&path, depth + 1, out);
            }
        } else if kind.is_file()
            && let Some(id) = video_id_from_name(&path)
        {
            out.push((path, id));
        }
    }
    Ok(())
}

/// `… [dQw4w9WgXcQ].m4a` -> `dQw4w9WgXcQ`, for audio files lofty can read.
fn video_id_from_name(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(ext.as_str(), "m4a" | "mp4" | "opus" | "ogg" | "mp3" | "flac") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let id = stem.strip_suffix(']')?.rsplit_once(" [")?.1;
    let valid = id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then(|| id.to_owned())
}

/// Track fields from the file's tags; `art` and the file stamp are filled by the caller.
fn read_file(path: &Path, video_id: String) -> Option<NewTrack> {
    let file = lofty::read_from_path(path)
        .inspect_err(|e| tracing::warn!(target: "ytmdl", "scan: {}: {e}", path.display()))
        .ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag());
    let text = |f: fn(&lofty::tag::Tag) -> Option<std::borrow::Cow<'_, str>>| {
        tag.and_then(f).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    };
    let title = text(|t| t.title()).unwrap_or_else(|| {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        stem.rsplit_once(" [").map_or(stem, |(title, _)| title).to_owned()
    });
    let artists = tag.map(ytmdl_core::tag::artists).unwrap_or_default();
    let album_artist = tag
        .and_then(|t| t.get_string(ItemKey::AlbumArtist))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| artists.first().cloned())
        .unwrap_or_default();
    let url = text(|t| t.comment())
        .filter(|c| c.starts_with("https://") && c.contains(&video_id))
        .unwrap_or_else(|| format!("https://music.youtube.com/watch?v={video_id}"));
    let duration = file.properties().duration().as_secs_f64();
    Some(NewTrack {
        video_id,
        url,
        path: path.to_owned(),
        title,
        artists,
        album: text(|t| t.album()),
        album_artist,
        track_number: tag.and_then(|t| t.track()),
        disc_number: tag.and_then(|t| t.disk()),
        year: tag.and_then(|t| t.date()).map(|d| i32::from(d.year)),
        duration_secs: (duration > 0.0).then_some(duration),
        art: None,
        gain: tag.and_then(ytmdl_core::loudness::from_tag),
        file_size: 0,
        file_mtime: 0,
    })
}

/// The embedded front cover (or else the first picture).
pub(crate) fn read_cover(path: &Path) -> Option<Vec<u8>> {
    let file = lofty::read_from_path(path).ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag())?;
    let pictures = tag.pictures();
    let picture = pictures.iter().find(|p| p.pic_type() == PictureType::CoverFront).or_else(|| pictures.first())?;
    Some(picture.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::video_id_from_name;
    use std::path::Path;

    #[test]
    fn recognizes_ytmdl_file_names() {
        let id = |p: &str| video_id_from_name(Path::new(p));
        assert_eq!(id("/m/A/B/Song [dQw4w9WgXcQ].m4a").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(id("/m/A/B/Song [with] brackets [a-b_c1234Z9].opus").as_deref(), Some("a-b_c1234Z9"));
        assert_eq!(id("/m/Song [dQw4w9WgXcQ].m4a.part"), None);
        assert_eq!(id("/m/Song [dQw4w9WgXcQ].temp.m4a"), None);
        assert_eq!(id("/m/Song [short].m4a"), None);
        assert_eq!(id("/m/Song.m4a"), None);
        assert_eq!(id("/m/Cover [dQw4w9WgXcQ].jpg"), None);
    }
}
