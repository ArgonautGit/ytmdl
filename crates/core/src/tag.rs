//! Writing and reading tags with lofty (pure Rust; no ffmpeg needed).

use std::fmt::Display;
use std::path::Path;

use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::items::Timestamp;
use lofty::tag::{ItemKey, Tag};
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
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u32>,
    pub disk: Option<u32>,
    pub year: Option<u16>,
    pub comment: Option<String>,
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
        report.album = tag.album().map(|s| s.into_owned());
        report.album_artist = tag.get_string(ItemKey::AlbumArtist).map(str::to_owned);
        report.track = tag.track();
        report.disk = tag.disk();
        report.year = tag.date().map(|d| d.year);
        report.comment = tag.comment().map(|s| s.into_owned());
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
