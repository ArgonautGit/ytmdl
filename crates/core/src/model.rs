//! The subset of yt-dlp's info dict we use. Unknown fields are ignored so new
//! yt-dlp releases can add fields freely.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Info {
    #[serde(rename = "_type")]
    pub kind: Option<String>,
    pub id: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub webpage_url: Option<String>,
    pub track: Option<String>,
    pub artist: Option<String>,
    pub artists: Option<Vec<String>>,
    pub creators: Option<Vec<String>>,
    pub album: Option<String>,
    pub album_artists: Option<Vec<String>>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub release_year: Option<i32>,
    pub release_date: Option<String>,
    pub upload_date: Option<String>,
    pub uploader: Option<String>,
    pub channel: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub thumbnails: Vec<Thumbnail>,
    #[serde(default)]
    pub entries: Vec<Info>,
    #[serde(default)]
    pub requested_downloads: Vec<RequestedDownload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Thumbnail {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub preference: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RequestedDownload {
    pub filepath: Option<PathBuf>,
}

/// A search result or collection member (from flat extraction).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub url: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub thumbnail: Option<String>,
}

/// Everything we know about one track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackMeta {
    pub id: String,
    pub url: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artists: Vec<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<i32>,
    pub duration_secs: Option<f64>,
    /// Best cover art candidates, most preferred first.
    pub cover_urls: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionKind {
    Album,
    Playlist,
    Search,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Resolved {
    Track(TrackMeta),
    Collection {
        title: String,
        kind: CollectionKind,
        entries: Vec<Entry>,
    },
}

/// A finished download.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Downloaded {
    pub path: PathBuf,
    pub meta: TrackMeta,
    /// Whether tags (and cover art, if found) were written.
    pub tagged: bool,
}

impl Info {
    fn primary_artists(&self) -> Vec<String> {
        if let Some(a) = self.artists.as_ref().filter(|a| !a.is_empty()) {
            return dedup_artists(a.clone());
        }
        if let Some(a) = self.creators.as_ref().filter(|a| !a.is_empty()) {
            return dedup_artists(a.clone());
        }
        if let Some(a) = self.artist.as_ref() {
            return dedup_artists(a.split(", ").map(str::to_owned).collect());
        }
        self.channel
            .clone()
            .or_else(|| self.uploader.clone())
            .map(|c| vec![c.trim_end_matches(" - Topic").to_owned()])
            .unwrap_or_default()
    }

    fn page_url(&self) -> String {
        self.webpage_url
            .clone()
            .or_else(|| self.url.clone())
            .or_else(|| self.id.as_ref().map(|id| format!("https://music.youtube.com/watch?v={id}")))
            .unwrap_or_default()
    }

    pub(crate) fn to_entry(&self) -> Option<Entry> {
        let id = self.id.clone()?;
        Some(Entry {
            url: self.page_url(),
            title: self.track.clone().or_else(|| self.title.clone()).unwrap_or_else(|| id.clone()),
            artists: self.primary_artists(),
            album: self.album.clone(),
            duration_secs: self.duration,
            thumbnail: self.cover_urls().into_iter().next(),
            id,
        })
    }

    pub(crate) fn to_track(&self) -> TrackMeta {
        let id = self.id.clone().unwrap_or_default();
        let year = self.release_year.or_else(|| {
            self.release_date
                .as_deref()
                .or(self.upload_date.as_deref())
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok())
        });
        TrackMeta {
            url: self.page_url(),
            title: self.track.clone().or_else(|| self.title.clone()).unwrap_or_else(|| id.clone()),
            artists: self.primary_artists(),
            album: self.album.clone(),
            album_artists: self.album_artists.clone().unwrap_or_default(),
            track_number: self.track_number,
            disc_number: self.disc_number,
            year,
            duration_secs: self.duration,
            cover_urls: self.cover_urls(),
            id,
        }
    }

    /// Cover candidates: square YouTube Music art first (upsized), then the
    /// largest JPEG video thumbnails. WebP is skipped (MP4 cover art can't hold it).
    pub(crate) fn cover_urls(&self) -> Vec<String> {
        let mut square = Vec::new();
        let mut video: Vec<&Thumbnail> = Vec::new();
        for t in &self.thumbnails {
            if t.url.contains("googleusercontent.com") || t.url.contains("ggpht.com") {
                square.push(upsize_art(&t.url));
            } else if !t.url.contains("webp") {
                video.push(t);
            }
        }
        video.sort_by_key(|t| {
            std::cmp::Reverse((
                t.width.unwrap_or(0) * t.height.unwrap_or(0),
                t.preference.unwrap_or(i64::MIN),
            ))
        });
        square.dedup();
        let mut out = square;
        out.extend(video.into_iter().map(|t| t.url.clone()));
        if out.is_empty()
            && let Some(t) = &self.thumbnail
            && !t.contains("webp")
        {
            out.push(t.clone());
        }
        out
    }
}

/// `...=w120-h120-l90-rj` -> `...=w1200-h1200-l90-rj` (Google image sizing params).
fn upsize_art(url: &str) -> String {
    match url.rsplit_once('=') {
        Some((base, params)) if params.starts_with('w') || params.starts_with('s') => {
            let rest: Vec<&str> = params
                .split('-')
                .filter(|p| !(p.starts_with('w') || p.starts_with('h') || p.starts_with('s')))
                .collect();
            let mut sized = vec!["w1200", "h1200"];
            sized.extend(rest);
            format!("{base}={}", sized.join("-"))
        }
        _ => url.to_owned(),
    }
}

/// YouTube Music sometimes lists a truncated duplicate of an artist
/// (`["Kevin MacLeod", "Kevin"]`); drop names that are a leading word run of another.
fn dedup_artists(mut artists: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    artists.retain(|a| !a.trim().is_empty() && seen.insert(a.clone()));
    let all = artists.clone();
    artists.retain(|a| !all.iter().any(|b| b != a && b.starts_with(a.as_str()) && b[a.len()..].starts_with(' ')));
    artists
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsizes_google_art() {
        assert_eq!(
            upsize_art("https://lh3.googleusercontent.com/abc=w120-h120-l90-rj"),
            "https://lh3.googleusercontent.com/abc=w1200-h1200-l90-rj"
        );
        assert_eq!(
            upsize_art("https://i.ytimg.com/vi/x/hqdefault.jpg"),
            "https://i.ytimg.com/vi/x/hqdefault.jpg"
        );
    }

    #[test]
    fn maps_music_track() {
        let info: Info = serde_json::from_value(serde_json::json!({
            "id": "abc",
            "title": "Song (Official Audio)",
            "track": "Song",
            "artists": ["A", "B"],
            "album": "Record",
            "release_year": 2021,
            "channel": "A - Topic",
            "webpage_url": "https://www.youtube.com/watch?v=abc",
            "thumbnails": [
                {"url": "https://i.ytimg.com/vi_webp/abc/maxresdefault.webp", "width": 1280, "height": 720},
                {"url": "https://i.ytimg.com/vi/abc/hqdefault.jpg", "width": 480, "height": 360},
                {"url": "https://i.ytimg.com/vi/abc/maxresdefault.jpg", "width": 1280, "height": 720},
                {"url": "https://lh3.googleusercontent.com/x=w60-h60-l90-rj"}
            ],
            "some_future_field": {"ignored": true}
        }))
        .unwrap();
        let t = info.to_track();
        assert_eq!(t.title, "Song");
        assert_eq!(t.artists, ["A", "B"]);
        assert_eq!(t.album.as_deref(), Some("Record"));
        assert_eq!(t.year, Some(2021));
        assert_eq!(
            t.cover_urls,
            [
                "https://lh3.googleusercontent.com/x=w1200-h1200-l90-rj",
                "https://i.ytimg.com/vi/abc/maxresdefault.jpg",
                "https://i.ytimg.com/vi/abc/hqdefault.jpg",
            ]
        );
    }

    #[test]
    fn falls_back_to_channel_without_topic_suffix() {
        let info: Info = serde_json::from_value(serde_json::json!({
            "id": "abc", "title": "T", "channel": "Band - Topic", "upload_date": "20190102"
        }))
        .unwrap();
        let t = info.to_track();
        assert_eq!(t.artists, ["Band"]);
        assert_eq!(t.year, Some(2019));
    }

    #[test]
    fn drops_truncated_artist_duplicates() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::dedup_artists(v(&["Kevin MacLeod", "Kevin"])), v(&["Kevin MacLeod"]));
        assert_eq!(super::dedup_artists(v(&["Kevin", "Kevin", "Kevinx"])), v(&["Kevin", "Kevinx"]));
        assert_eq!(super::dedup_artists(v(&["Daft Punk", "Pharrell Williams"])), v(&["Daft Punk", "Pharrell Williams"]));
    }
}
