//! Writing and reading tags with lofty (pure Rust; no ffmpeg needed).

use std::fmt::Display;
use std::path::Path;

use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::items::Timestamp;
use lofty::tag::{ItemKey, ItemValue, Tag, TagItem};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::TrackMeta;

/// Cover art bytes with a format MP4/Ogg tags can hold.
pub struct Cover {
    pub data: Vec<u8>,
    pub mime: MimeType,
}

impl Cover {
    /// Accepts JPEG and PNG, detected from the data itself.
    pub fn sniff(data: Vec<u8>) -> Option<Cover> {
        let mime = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            MimeType::Jpeg
        } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            MimeType::Png
        } else {
            return None;
        };
        Some(Cover { data, mime })
    }
}

/// Whether lofty can write tags into files with this extension.
pub fn can_tag(ext: &str) -> bool {
    matches!(ext, "m4a" | "mp4" | "opus" | "ogg" | "mp3" | "flac")
}

/// Writes each artist as its own `ARTISTS` value (MusicBrainz Picard's
/// convention), next to the display string in the artist tag. The display
/// string joins them with ", ", which can't be split back reliably: "Tyler,
/// The Creator" is one artist.
fn set_artists(tag: &mut Tag, artists: &[String]) {
    tag.remove_key(ItemKey::TrackArtists);
    for artist in artists {
        tag.push(TagItem::new(ItemKey::TrackArtists, ItemValue::Text(artist.clone())));
    }
}

/// The track's artists: the `ARTISTS` values when the file has them, else the
/// artist tag split at ", " (files tagged before ytmdl wrote `ARTISTS`).
pub fn artists(tag: &Tag) -> Vec<String> {
    let listed: Vec<String> =
        tag.get_strings(ItemKey::TrackArtists).map(str::trim).filter(|a| !a.is_empty()).map(str::to_owned).collect();
    if !listed.is_empty() {
        return listed;
    }
    let artist = tag.artist().map(|a| a.trim().to_owned()).unwrap_or_default();
    artist.split(", ").filter(|a| !a.is_empty()).map(str::to_owned).collect()
}

/// Adds the `ARTISTS` values to a file tagged without them, leaving the rest of
/// its tags alone. Returns whether the file was changed.
pub fn add_artists(path: &Path, artists: &[String]) -> Result<bool> {
    let err = |e: &dyn Display| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    };
    let mut file = lofty::read_from_path(path).map_err(|e| err(&e))?;
    let Some(tag) = file.primary_tag_mut() else { return Ok(false) };
    if tag.get_strings(ItemKey::TrackArtists).next().is_some() {
        return Ok(false);
    }
    set_artists(tag, artists);
    file.save_to_path(path, WriteOptions::default()).map_err(|e| err(&e))?;
    Ok(true)
}

/// Sets a file's title and artists, leaving its other tags alone. An album
/// artist that was the first artist becomes the new first artist.
pub fn retitle(path: &Path, title: &str, names: &[String]) -> Result<()> {
    let err = |e: &dyn Display| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    };
    let mut file = lofty::read_from_path(path).map_err(|e| err(&e))?;
    if file.primary_tag().is_none() {
        let tag_type = file.primary_tag_type();
        file.insert_tag(Tag::new(tag_type));
    }
    let tag = file.primary_tag_mut().expect("primary tag was just inserted");
    let first = artists(tag).into_iter().next();
    tag.set_title(title.to_owned());
    if names.is_empty() {
        tag.remove_artist();
    } else {
        tag.set_artist(names.join(", "));
    }
    set_artists(tag, names);
    if let (Some(first), Some(new)) = (first, names.first())
        && tag.get_string(ItemKey::AlbumArtist) == Some(first.as_str())
    {
        tag.insert_text(ItemKey::AlbumArtist, new.clone());
    }
    file.save_to_path(path, WriteOptions::default()).map_err(|e| err(&e))?;
    Ok(())
}

pub fn write_tags(path: &Path, meta: &TrackMeta, cover: Option<Cover>) -> Result<()> {
    write_tags_and_lyrics(path, meta, cover, None)
}

/// [`write_tags`], plus the lyrics when there are some (see [`crate::lyrics`]).
pub fn write_tags_and_lyrics(path: &Path, meta: &TrackMeta, cover: Option<Cover>, lyrics: Option<&str>) -> Result<()> {
    let err = |e: &dyn Display| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    };
    let mut file = lofty::read_from_path(path).map_err(|e| err(&e))?;
    if file.primary_tag().is_none() {
        let tag_type = file.primary_tag_type();
        file.insert_tag(Tag::new(tag_type));
    }
    let tag = file.primary_tag_mut().expect("primary tag was just inserted");

    tag.set_title(meta.title.clone());
    if !meta.artists.is_empty() {
        tag.set_artist(meta.artists.join(", "));
    }
    set_artists(tag, &meta.artists);
    if let Some(album) = &meta.album {
        tag.set_album(album.clone());
    }
    if !meta.album_artists.is_empty() {
        tag.insert_text(ItemKey::AlbumArtist, meta.album_artists.join(", "));
    }
    if let Some(n) = meta.track_number {
        tag.set_track(n);
    }
    if let Some(n) = meta.disc_number {
        tag.set_disk(n);
    }
    if let Some(year) = meta.year.and_then(|y| u16::try_from(y).ok()) {
        tag.set_date(Timestamp {
            year,
            ..Timestamp::default()
        });
    }
    tag.set_comment(meta.url.clone());
    if let Some(lyrics) = lyrics {
        tag.insert_text(ItemKey::Lyrics, lyrics.to_owned());
    }
    if let Some(cover) = cover {
        tag.remove_picture_type(PictureType::CoverFront);
        tag.push_picture(
            Picture::unchecked(cover.data)
                .pic_type(PictureType::CoverFront)
                .mime_type(cover.mime)
                .build(),
        );
    }
    file.save_to_path(path, WriteOptions::default()).map_err(|e| err(&e))?;
    Ok(())
}

/// The lyrics stored in the file, if any.
pub fn read_lyrics(path: &Path) -> Result<Option<String>> {
    let file = lofty::read_from_path(path).map_err(|e| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    let tag = file.primary_tag().or_else(|| file.first_tag());
    Ok(tag.and_then(|t| t.get_string(ItemKey::Lyrics)).map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned))
}

/// Stores lyrics in a file, leaving its other tags alone.
pub fn write_lyrics(path: &Path, lyrics: &str) -> Result<()> {
    let err = |e: &dyn Display| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    };
    let mut file = lofty::read_from_path(path).map_err(|e| err(&e))?;
    if file.primary_tag().is_none() {
        let tag_type = file.primary_tag_type();
        file.insert_tag(Tag::new(tag_type));
    }
    let tag = file.primary_tag_mut().expect("primary tag was just inserted");
    tag.insert_text(ItemKey::Lyrics, lyrics.to_owned());
    file.save_to_path(path, WriteOptions::default()).map_err(|e| err(&e))?;
    Ok(())
}

/// What a file contains, as read back by lofty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagReport {
    pub file_type: String,
    pub duration_secs: f64,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// Each artist, as [`artists`] reads them.
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u32>,
    pub disk: Option<u32>,
    pub year: Option<u16>,
    pub comment: Option<String>,
    /// ReplayGain track gain (dB) and peak.
    pub replaygain: Option<crate::loudness::Gain>,
    /// Whether the file holds lyrics.
    pub has_lyrics: bool,
    /// (mime type, size in bytes) of each embedded picture.
    pub pictures: Vec<(String, usize)>,
}

pub fn read_tags(path: &Path) -> Result<TagReport> {
    let file = lofty::read_from_path(path).map_err(|e| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    let props = file.properties();
    let mut report = TagReport {
        file_type: format!("{:?}", file.file_type()),
        duration_secs: props.duration().as_secs_f64(),
        bitrate_kbps: props.audio_bitrate(),
        sample_rate: props.sample_rate(),
        channels: props.channels(),
        ..TagReport::default()
    };
    if let Some(tag) = file.primary_tag() {
        report.title = tag.title().map(|s| s.into_owned());
        report.artist = tag.artist().map(|s| s.into_owned());
        report.artists = artists(tag);
        report.album = tag.album().map(|s| s.into_owned());
        report.album_artist = tag.get_string(ItemKey::AlbumArtist).map(str::to_owned);
        report.track = tag.track();
        report.disk = tag.disk();
        report.year = tag.date().map(|d| d.year);
        report.comment = tag.comment().map(|s| s.into_owned());
        report.replaygain = crate::loudness::from_tag(tag);
        report.has_lyrics = tag.get_string(ItemKey::Lyrics).is_some_and(|l| !l.trim().is_empty());
        report.pictures = tag
            .pictures()
            .iter()
            .map(|p| {
                let mime = p.mime_type().map(|m| m.as_str().to_owned()).unwrap_or_default();
                (mime, p.data().len())
            })
            .collect();
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// A copy of `.deps/fixtures/tone.m4a` (`ytmdl-cli record-fixtures`), or
    /// `None` to skip the test without it.
    fn fixture(name: &str) -> Option<PathBuf> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deps/fixtures/tone.m4a");
        if !fixture.exists() {
            eprintln!("skipping: {} is missing", fixture.display());
            return None;
        }
        let path = std::env::temp_dir().join(format!("ytmdl-tag-{}-{name}.m4a", std::process::id()));
        std::fs::copy(&fixture, &path).unwrap();
        Some(path)
    }

    fn meta(artists: &[&str]) -> TrackMeta {
        TrackMeta {
            id: "abcdefghijk".into(),
            url: "https://music.youtube.com/watch?v=abcdefghijk".into(),
            title: "Song".into(),
            artists: artists.iter().map(|a| a.to_string()).collect(),
            album: None,
            album_artists: Vec::new(),
            track_number: None,
            disc_number: None,
            year: None,
            duration_secs: None,
            cover_urls: Vec::new(),
        }
    }

    #[test]
    fn keeps_artists_with_commas_apart() {
        let Some(path) = fixture("commas") else { return };
        write_tags(&path, &meta(&["Tyler, The Creator", "Kali Uchis"]), None).unwrap();
        let report = read_tags(&path).unwrap();
        assert_eq!(report.artist.as_deref(), Some("Tyler, The Creator, Kali Uchis"));
        assert_eq!(report.artists, ["Tyler, The Creator", "Kali Uchis"]);

        // Tagging again replaces the list rather than adding to it.
        write_tags(&path, &meta(&["Earth, Wind & Fire"]), None).unwrap();
        assert_eq!(read_tags(&path).unwrap().artists, ["Earth, Wind & Fire"]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn retitles_files() {
        let Some(path) = fixture("retitle") else { return };
        let mut m = meta(&["TaylorSwiftVEVO"]);
        m.album = Some("Singles".into());
        m.album_artists = vec!["TaylorSwiftVEVO".into()];
        m.year = Some(2022);
        write_tags(&path, &m, None).unwrap();
        retitle(&path, "Anti-Hero", &["Taylor Swift".into(), "Guest".into()]).unwrap();
        let report = read_tags(&path).unwrap();
        assert_eq!(report.title.as_deref(), Some("Anti-Hero"));
        assert_eq!(report.artists, ["Taylor Swift", "Guest"]);
        assert_eq!(report.album_artist.as_deref(), Some("Taylor Swift"));
        assert_eq!((report.album.as_deref(), report.year), (Some("Singles"), Some(2022)));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn adds_artists_to_older_files() {
        let Some(path) = fixture("older") else { return };
        write_tags(&path, &meta(&["Tyler, The Creator"]), None).unwrap();
        // As tagged before ARTISTS was written.
        let mut file = lofty::read_from_path(&path).unwrap();
        file.primary_tag_mut().unwrap().remove_key(ItemKey::TrackArtists);
        file.save_to_path(&path, WriteOptions::default()).unwrap();
        assert_eq!(read_tags(&path).unwrap().artists, ["Tyler", "The Creator"]);

        assert!(add_artists(&path, &["Tyler, The Creator".into()]).unwrap());
        let report = read_tags(&path).unwrap();
        assert_eq!(report.artists, ["Tyler, The Creator"]);
        assert_eq!(report.title.as_deref(), Some("Song"));
        assert!(!add_artists(&path, &["Tyler, The Creator".into()]).unwrap());
        std::fs::remove_file(&path).unwrap();
    }
}
