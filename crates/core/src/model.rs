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
    /// Row label of a YouTube Music search result ("song", "album", "ep", ...),
    /// added by the bridge.
    pub ytmdl_kind: Option<String>,
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
    /// Small (list-row sized) art or video thumbnail; see [`art_url`] for other sizes.
    pub thumbnail: Option<String>,
    /// YouTube Music's label for the result: "song", "video", "album", "ep", "single".
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    /// Position in the album it was listed in (YouTube has no track numbers).
    #[serde(default)]
    pub track_number: Option<u32>,
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

impl TrackMeta {
    /// With the title and artists tidied (see [`crate::titles::tidy`]).
    pub fn tidied(mut self) -> Self {
        (self.title, self.artists) = crate::titles::tidy(&self.title, &self.artists);
        self
    }
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

/// Where a YouTube Music radio starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RadioSeed {
    /// Songs like this one (its video id), starting with it.
    Song(String),
    /// A mix YouTube Music names, such as an artist's.
    Mix { playlist_id: String, params: Option<String> },
}

impl RadioSeed {
    /// The bridge's `music_radio` request.
    pub(crate) fn request(&self, limit: usize) -> serde_json::Value {
        match self {
            RadioSeed::Song(id) => serde_json::json!({ "video_id": id, "limit": limit }),
            RadioSeed::Mix { playlist_id, params } => {
                serde_json::json!({ "playlist_id": playlist_id, "params": params, "limit": limit })
            }
        }
    }
}

/// A YouTube Music artist page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistPage {
    /// The artist's channel id.
    pub id: String,
    /// Empty if the page doesn't say.
    pub name: String,
    /// The artist's picture (a wide banner; [`art_url`] crops it square).
    pub art: Option<String>,
    /// "7.97M monthly audience", or "20.5M subscribers".
    pub audience: Option<String>,
    pub description: Option<String>,
    /// The artist's mix.
    pub radio: Option<RadioSeed>,
    pub sections: Vec<ArtistSection>,
}

/// One list of an artist page, as YouTube Music shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistSection {
    /// YouTube Music's heading: "Top songs", "Albums", "Fans might also like".
    pub title: String,
    pub kind: SectionKind,
    pub entries: Vec<Entry>,
    /// The whole list, when the page shows part of it.
    pub more: Option<More>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionKind {
    Songs,
    Videos,
    /// Albums, singles and EPs.
    Albums,
    Playlists,
    /// Other artists.
    Artists,
}

/// Where the whole of an artist page's section is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum More {
    /// A playlist (all of the artist's songs, say), opened like any other.
    Playlist(String),
    /// A list [`crate::Downloader::browse`] reads (all of the artist's albums).
    Browse { browse_id: String, params: Option<String> },
}

/// The bridge's `music_radio` and `music_browse` answer.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct EntryList {
    #[serde(default)]
    pub entries: Vec<Info>,
}

impl EntryList {
    pub(crate) fn entries(&self) -> Vec<Entry> {
        self.entries.iter().filter_map(Info::to_entry).collect()
    }
}

/// The bridge's `music_artist` answer.
#[derive(Debug, Deserialize)]
pub(crate) struct RawArtist {
    id: String,
    name: Option<String>,
    #[serde(default)]
    thumbnails: Vec<Thumbnail>,
    subscribers: Option<String>,
    audience: Option<String>,
    description: Option<String>,
    radio: Option<RawMix>,
    #[serde(default)]
    sections: Vec<RawSection>,
}

#[derive(Debug, Deserialize)]
struct RawMix {
    playlist_id: String,
    params: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSection {
    title: Option<String>,
    /// Unknown kinds (from a newer bridge) are left out.
    kind: String,
    more: Option<RawMore>,
    #[serde(default)]
    entries: Vec<Info>,
}

#[derive(Debug, Deserialize)]
struct RawMore {
    browse_id: String,
    params: Option<String>,
}

impl RawArtist {
    pub(crate) fn into_page(self) -> ArtistPage {
        let art = self
            .thumbnails
            .iter()
            .max_by_key(|t| t.width.unwrap_or(0) * t.height.unwrap_or(0))
            .map(|t| t.url.clone());
        let sections = self
            .sections
            .into_iter()
            .filter_map(|s| {
                let kind = serde_json::from_value(serde_json::Value::String(s.kind)).ok()?;
                let entries: Vec<Entry> = s.entries.iter().filter_map(Info::to_entry).collect();
                (!entries.is_empty()).then(|| ArtistSection {
                    title: s.title.unwrap_or_default(),
                    kind,
                    entries,
                    more: s.more.map(|m| match m.browse_id.strip_prefix("VL") {
                        Some(list) => More::Playlist(format!("https://music.youtube.com/playlist?list={list}")),
                        None => More::Browse { browse_id: m.browse_id, params: m.params },
                    }),
                })
            })
            .collect();
        ArtistPage {
            id: self.id,
            name: self.name.unwrap_or_default(),
            art,
            audience: self.audience.or_else(|| self.subscribers.map(|s| format!("{s} subscribers"))),
            description: self.description,
            radio: self.radio.map(|r| RadioSeed::Mix { playlist_id: r.playlist_id, params: r.params }),
            sections,
        }
    }
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
            thumbnail: self.cover_urls().into_iter().next().map(|u| art_url(&u, LIST_ART_PX)),
            kind: self.ytmdl_kind.clone(),
            year: self.release_year,
            track_number: None,
            id,
        })
    }

    pub(crate) fn to_track(&self) -> TrackMeta {
        self.untidied_track().tidied()
    }

    /// The track as the upload names it, before [`TrackMeta::tidied`].
    pub(crate) fn untidied_track(&self) -> TrackMeta {
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
                square.push(art_url(&t.url, COVER_ART_PX));
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

/// Edge of the art embedded in files.
const COVER_ART_PX: u32 = 1200;
/// Edge of the art in [`Entry::thumbnail`]: enough for a list row on a 3.5x screen.
const LIST_ART_PX: u32 = 226;

/// YouTube Music art at `px` square: `...=w120-h120-l90-rj` -> `...=w544-h544-l90-rj`
/// (Google image sizing params). Other URLs (video thumbnails) come back unchanged.
pub fn art_url(url: &str, px: u32) -> String {
    if !(url.contains("googleusercontent.com") || url.contains("ggpht.com")) {
        return url.to_owned();
    }
    match url.rsplit_once('=') {
        Some((base, params)) if params.starts_with('w') || params.starts_with('s') => {
            let rest = params
                .split('-')
                .filter(|p| !(p.starts_with('w') || p.starts_with('h') || p.starts_with('s')));
            let sized: Vec<String> = [format!("w{px}"), format!("h{px}")].into_iter().chain(rest.map(str::to_owned)).collect();
            format!("{base}={}", sized.join("-"))
        }
        _ => url.to_owned(),
    }
}

/// One address per playlist, whatever link it came from: the `list` parameter
/// of a YouTube or YouTube Music link, as a YouTube Music playlist URL. `None`
/// when the link names no playlist.
pub fn playlist_url(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    let id = query.split('&').find_map(|p| p.strip_prefix("list="))?;
    let valid = !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then(|| format!("https://music.youtube.com/playlist?list={id}"))
}

/// The artist a YouTube Music artist link names (`/channel/UC…` or
/// `/browse/UC…`): their channel id.
pub fn artist_id(url: &str) -> Option<String> {
    let (host, path) = url.split_once("://")?.1.split_once('/')?;
    if host != "music.youtube.com" {
        return None;
    }
    let path = path.split(['?', '#']).next()?;
    let id = path.strip_prefix("channel/").or_else(|| path.strip_prefix("browse/"))?.trim_end_matches('/');
    let valid = id.len() > 2 && id.starts_with("UC") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then(|| id.to_owned())
}

/// The link in text shared from another app ("Check this out https://…").
/// A song link keeps only its video: YouTube Music adds the list it was
/// playing from (`list=RD…`), which would open that mix instead of the song.
pub fn shared_link(text: &str) -> Option<String> {
    let url = text
        .split_whitespace()
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))?
        .trim_end_matches(['.', ',', ')', '"', '\'']);
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let valid = |id: &str| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if host == "youtu.be" {
        let id = rest[host.len()..].trim_start_matches('/').split(['?', '#', '/']).next()?;
        return valid(id).then(|| format!("https://www.youtube.com/watch?v={id}"));
    }
    let query = url.split_once('?').map_or("", |(_, q)| q.split('#').next().unwrap_or(""));
    match query.split('&').find_map(|p| p.strip_prefix("v=")) {
        Some(id) if valid(id) => Some(format!("https://{host}/watch?v={id}")),
        Some(_) => None,
        None => Some(url.to_owned()),
    }
}

/// YouTube names album playlists "Album - <title>" (also "EP - ", "Single - ").
pub(crate) fn album_title(title: &str) -> String {
    ["Album - ", "EP - ", "Single - "]
        .iter()
        .find_map(|p| title.strip_prefix(p))
        .unwrap_or(title)
        .to_owned()
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
    fn resizes_google_art() {
        assert_eq!(
            art_url("https://lh3.googleusercontent.com/abc=w120-h120-l90-rj", 1200),
            "https://lh3.googleusercontent.com/abc=w1200-h1200-l90-rj"
        );
        assert_eq!(
            art_url("https://yt3.googleusercontent.com/abc=w60-h60-l90-rj", 226),
            "https://yt3.googleusercontent.com/abc=w226-h226-l90-rj"
        );
        let video = "https://i.ytimg.com/vi/x/hqdefault.jpg?sqp=-oaymwE&rs=w123";
        assert_eq!(art_url(video, 1200), video);
    }

    #[test]
    fn finds_playlist_ids() {
        let canonical = "https://music.youtube.com/playlist?list=PLx-y_1";
        for url in [
            "https://music.youtube.com/playlist?list=PLx-y_1",
            "https://www.youtube.com/playlist?list=PLx-y_1&si=abc",
            "https://music.youtube.com/watch?v=abc&list=PLx-y_1#x",
            "https://youtube.com/watch?v=abc&feature=share&list=PLx-y_1",
        ] {
            assert_eq!(playlist_url(url).as_deref(), Some(canonical), "{url}");
        }
        assert_eq!(playlist_url("https://music.youtube.com/watch?v=abc"), None);
        assert_eq!(playlist_url("https://music.youtube.com/playlist?list="), None);
        assert_eq!(playlist_url("https://www.youtube.com/playlist?list=PL<script>"), None);
    }

    #[test]
    fn finds_shared_links() {
        let song = Some("https://music.youtube.com/watch?v=abc-_12");
        assert_eq!(shared_link("https://music.youtube.com/watch?v=abc-_12&si=xyz").as_deref(), song);
        assert_eq!(shared_link("https://music.youtube.com/watch?v=abc-_12&list=RDAMVMabc").as_deref(), song);
        assert_eq!(shared_link("Listen to this: https://music.youtube.com/watch?v=abc-_12.").as_deref(), song);
        assert_eq!(
            shared_link("https://youtu.be/abc-_12?si=xyz").as_deref(),
            Some("https://www.youtube.com/watch?v=abc-_12")
        );
        let playlist = "https://music.youtube.com/playlist?list=PLx-y_1&si=abc";
        assert_eq!(shared_link(playlist).as_deref(), Some(playlist));
        let album = "https://music.youtube.com/browse/MPREb_abc";
        assert_eq!(shared_link(&format!("Album {album}")).as_deref(), Some(album));
        assert_eq!(shared_link("no link here"), None);
        assert_eq!(shared_link("https://music.youtube.com/watch?v=<x>"), None);
    }

    #[test]
    fn finds_artist_ids() {
        let id = Some("UC9ouIc5bVUPVzZ0X_JETPEA");
        assert_eq!(artist_id("https://music.youtube.com/channel/UC9ouIc5bVUPVzZ0X_JETPEA").as_deref(), id);
        assert_eq!(artist_id("https://music.youtube.com/browse/UC9ouIc5bVUPVzZ0X_JETPEA?si=x").as_deref(), id);
        assert_eq!(artist_id("https://music.youtube.com/channel/UC9ouIc5bVUPVzZ0X_JETPEA/").as_deref(), id);
        // An album, a YouTube channel (not an artist page), no id.
        assert_eq!(artist_id("https://music.youtube.com/browse/MPREb_abc"), None);
        assert_eq!(artist_id("https://www.youtube.com/channel/UC9ouIc5bVUPVzZ0X_JETPEA"), None);
        assert_eq!(artist_id("https://music.youtube.com/channel/"), None);
        assert_eq!(artist_id("https://music.youtube.com/channel/UC<x>"), None);
    }

    #[test]
    fn asks_for_radios() {
        assert_eq!(
            RadioSeed::Song("abc".into()).request(50),
            serde_json::json!({ "video_id": "abc", "limit": 50 })
        );
        let mix = RadioSeed::Mix { playlist_id: "RDEMx".into(), params: Some("wAEB".into()) };
        assert_eq!(mix.request(20), serde_json::json!({ "playlist_id": "RDEMx", "params": "wAEB", "limit": 20 }));
    }

    #[test]
    fn maps_artist_pages() {
        let raw: RawArtist = serde_json::from_value(serde_json::json!({
            "id": "UCx",
            "name": "Pentatonix",
            "thumbnails": [
                {"url": "https://yt3.googleusercontent.com/p=w540-h225-p-l90-rj", "width": 540, "height": 225},
                {"url": "https://yt3.googleusercontent.com/p=w1440-h600-p-l90-rj", "width": 1440, "height": 600}
            ],
            "subscribers": "20.5M",
            "audience": null,
            "radio": {"playlist_id": "RDEMx", "params": "wAEB"},
            "sections": [
                {"title": "Top songs", "kind": "songs",
                 "more": {"browse_id": "VLOLAK5uy_x", "params": "ggMCCAI%3D"},
                 "entries": [{"_type": "url", "id": "v1", "url": "https://music.youtube.com/watch?v=v1",
                              "title": "Jolene", "artists": ["Pentatonix"], "ytmdl_kind": "song"}]},
                {"title": "Albums", "kind": "albums",
                 "more": {"browse_id": "UCx", "params": "abc"},
                 "entries": [{"_type": "url", "id": "MPREb_1", "url": "https://music.youtube.com/browse/MPREb_1",
                              "title": "PTX", "release_year": 2014, "ytmdl_page": "album"}]},
                {"title": "Coming soon", "kind": "concerts", "entries": [{"id": "x", "title": "x"}]},
                {"title": "Empty", "kind": "videos", "entries": []}
            ]
        }))
        .unwrap();
        let page = raw.into_page();
        assert_eq!(page.name, "Pentatonix");
        assert_eq!(page.art.as_deref(), Some("https://yt3.googleusercontent.com/p=w1440-h600-p-l90-rj"));
        assert_eq!(page.audience.as_deref(), Some("20.5M subscribers"));
        assert_eq!(page.radio, Some(RadioSeed::Mix { playlist_id: "RDEMx".into(), params: Some("wAEB".into()) }));
        // Unknown kinds and empty sections are left out.
        assert_eq!(page.sections.len(), 2);
        let songs = &page.sections[0];
        assert_eq!((songs.title.as_str(), songs.kind), ("Top songs", SectionKind::Songs));
        assert_eq!(songs.more, Some(More::Playlist("https://music.youtube.com/playlist?list=OLAK5uy_x".into())));
        assert_eq!(songs.entries[0].title, "Jolene");
        let albums = &page.sections[1];
        assert_eq!(albums.more, Some(More::Browse { browse_id: "UCx".into(), params: Some("abc".into()) }));
        assert_eq!(albums.entries[0].url, "https://music.youtube.com/browse/MPREb_1");
        assert_eq!(albums.entries[0].year, Some(2014));
    }

    #[test]
    fn cleans_album_titles() {
        assert_eq!(album_title("Album - Action Cuts"), "Action Cuts");
        assert_eq!(album_title("EP - Four - Five"), "Four - Five");
        assert_eq!(album_title("Mixtape - Tape"), "Mixtape - Tape");
    }

    #[test]
    fn maps_enriched_music_search_row() {
        let info: Info = serde_json::from_value(serde_json::json!({
            "_type": "url", "id": "MPREb_x", "url": "https://music.youtube.com/browse/MPREb_x",
            "title": "Random Access Memories", "artists": ["Daft Punk"], "release_year": 2013,
            "ytmdl_kind": "album",
            "thumbnails": [
                {"url": "https://yt3.googleusercontent.com/a=w60-h60-l90-rj", "width": 60, "height": 60},
                {"url": "https://yt3.googleusercontent.com/a=w544-h544-l90-rj", "width": 544, "height": 544}
            ]
        }))
        .unwrap();
        let e = info.to_entry().unwrap();
        assert_eq!(e.title, "Random Access Memories");
        assert_eq!(e.artists, ["Daft Punk"]);
        assert_eq!(e.kind.as_deref(), Some("album"));
        assert_eq!(e.year, Some(2013));
        assert_eq!(e.thumbnail.as_deref(), Some("https://yt3.googleusercontent.com/a=w226-h226-l90-rj"));
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
    fn tidies_video_titles() {
        let info: Info = serde_json::from_value(serde_json::json!({
            "id": "abc", "title": "Taylor Swift - Anti-Hero (Official Music Video)", "channel": "TaylorSwiftVEVO"
        }))
        .unwrap();
        let t = info.to_track();
        assert_eq!(t.title, "Anti-Hero");
        assert_eq!(t.artists, ["Taylor Swift"]);
        let named = info.untidied_track();
        assert_eq!((named.title.as_str(), named.artists.as_slice()), ("Taylor Swift - Anti-Hero (Official Music Video)", &["TaylorSwiftVEVO".to_owned()][..]));
    }

    #[test]
    fn drops_truncated_artist_duplicates() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::dedup_artists(v(&["Kevin MacLeod", "Kevin"])), v(&["Kevin MacLeod"]));
        assert_eq!(super::dedup_artists(v(&["Kevin", "Kevin", "Kevinx"])), v(&["Kevin", "Kevinx"]));
        assert_eq!(super::dedup_artists(v(&["Daft Punk", "Pharrell Williams"])), v(&["Daft Punk", "Pharrell Williams"]));
    }
}
