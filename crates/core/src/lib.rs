//! Search and download music from YouTube / YouTube Music.
//!
//! Extraction is delegated to an embedded upstream yt-dlp (see [`runtime`]); this
//! crate adds a typed API, download progress/cancellation, and tagging.

pub mod error;
pub mod loudness;
pub mod lyrics;
pub mod model;
pub mod remux;
pub mod runtime;
pub mod selftest;
pub mod tag;
pub mod titles;

use std::path::PathBuf;

pub use error::{Error, Result};
pub use model::{
    ArtistPage, ArtistSection, CollectionKind, Downloaded, Entry, More, RadioSeed, Resolved, SectionKind, TrackMeta,
    art_url, artist_id, playlist_url, shared_link,
};
pub use runtime::{CancelToken, Channel, Progress, Runtime, RuntimeConfig, UpdateOutcome, VersionInfo};

use model::{EntryList, Info, RawArtist};

/// Largest cover image we will embed.
const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;

/// Prefer AAC in MP4 (taggable without ffmpeg), then any audio-only stream.
pub const DEFAULT_FORMAT: &str = "bestaudio[ext=m4a]/bestaudio";

/// Songs in a radio (YouTube Music gives about 50 at a time).
pub const RADIO_SONGS: usize = 50;

/// `<out>/<Artist>/<Album>/<Title> [<id>].<ext>`
pub const DEFAULT_TEMPLATE: &str =
    "%(album_artist,artists.0,creators.0,uploader|Unknown Artist)s/%(album|Singles)s/%(track,title)s [%(id)s].%(ext)s";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    /// YouTube Music "Songs" results.
    MusicSongs,
    /// YouTube Music "Albums" results.
    MusicAlbums,
    /// YouTube Music "Artists" results.
    MusicArtists,
    /// Regular YouTube search.
    YouTube,
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub output_dir: PathBuf,
    /// yt-dlp output template, relative to `output_dir`.
    pub template: String,
    /// yt-dlp format selector.
    pub format: String,
    pub write_tags: bool,
    pub embed_cover: bool,
    /// Extra raw yt-dlp arguments.
    pub extra_args: Vec<String>,
    /// Track number to tag when yt-dlp has none (see [`Entry::track_number`]).
    pub track_number: Option<u32>,
    /// Measure the loudness and tag the ReplayGain (see [`loudness`]).
    pub measure_loudness: bool,
    /// Look the lyrics up on LRCLIB and store them in the file (see [`lyrics`]).
    pub lyrics: bool,
}

impl DownloadOptions {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        DownloadOptions {
            output_dir: output_dir.into(),
            template: DEFAULT_TEMPLATE.into(),
            format: DEFAULT_FORMAT.into(),
            write_tags: true,
            embed_cover: true,
            extra_args: Vec::new(),
            track_number: None,
            measure_loudness: true,
            lyrics: true,
        }
    }
}

/// High-level operations on top of a started [`Runtime`].
#[derive(Clone)]
pub struct Downloader {
    rt: Runtime,
}

impl Downloader {
    pub fn new(rt: Runtime) -> Self {
        Downloader { rt }
    }

    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }

    pub async fn search(&self, query: &str, limit: usize, source: SearchSource) -> Result<Vec<Entry>> {
        let url = match source {
            SearchSource::YouTube => format!("ytsearch{limit}:{query}"),
            SearchSource::MusicSongs => format!("https://music.youtube.com/search?q={}#songs", encode_query(query)),
            SearchSource::MusicAlbums => format!("https://music.youtube.com/search?q={}#albums", encode_query(query)),
            SearchSource::MusicArtists => format!("https://music.youtube.com/search?q={}#artists", encode_query(query)),
        };
        let mut args = self.rt.base_args();
        args.extend(["--flat-playlist".into(), "--playlist-end".into(), limit.to_string()]);
        let info: Info = serde_json::from_str(&self.rt.extract_json(args, url).await?)?;
        Ok(info.entries.iter().filter_map(Info::to_entry).collect())
    }

    /// Track metadata, or the members of an album/playlist.
    pub async fn resolve(&self, url: &str) -> Result<Resolved> {
        self.resolve_as(url, None).await
    }

    /// Like [`Downloader::resolve`], for a link known to be an album or a
    /// playlist: all of an artist's songs are an `OLAK5uy_` playlist, which
    /// would otherwise pass for an album.
    pub async fn resolve_as(&self, url: &str, kind: Option<CollectionKind>) -> Result<Resolved> {
        let mut args = self.rt.base_args();
        args.push("--flat-playlist".into());
        let info: Info = serde_json::from_str(&self.rt.extract_json(args, url.to_owned()).await?)?;
        if info.kind.as_deref() == Some("playlist") {
            let kind = kind.unwrap_or(if url.contains("OLAK5uy_") || url.contains("/browse/MPREb") {
                CollectionKind::Album
            } else {
                CollectionKind::Playlist
            });
            let mut title = info.title.clone().unwrap_or_default();
            let mut entries: Vec<Entry> = info.entries.iter().filter_map(Info::to_entry).collect();
            if kind == CollectionKind::Album {
                title = model::album_title(&title);
                for (n, e) in (1..).zip(&mut entries) {
                    e.album.get_or_insert_with(|| title.clone());
                    e.track_number = Some(n);
                }
            }
            return Ok(Resolved::Collection { title, kind, entries });
        }
        Ok(Resolved::Track(info.to_track()))
    }

    /// YouTube Music's radio from `seed`: up to `limit` songs like it.
    pub async fn radio(&self, seed: &RadioSeed, limit: usize) -> Result<Vec<Entry>> {
        let json = self.rt.music_json("music_radio", self.rt.base_args(), seed.request(limit)).await?;
        Ok(serde_json::from_str::<EntryList>(&json)?.entries())
    }

    /// The YouTube Music page of the artist with channel id `id`.
    pub async fn artist(&self, id: &str) -> Result<ArtistPage> {
        let request = serde_json::json!({ "browse_id": id });
        let json = self.rt.music_json("music_artist", self.rt.base_args(), request).await?;
        Ok(serde_json::from_str::<RawArtist>(&json)?.into_page())
    }

    /// Up to `limit` entries of a [`More::Browse`] list (all of an artist's
    /// albums, say).
    pub async fn browse(&self, browse_id: &str, params: Option<&str>, limit: usize) -> Result<Vec<Entry>> {
        let request = serde_json::json!({ "browse_id": browse_id, "params": params, "limit": limit });
        let json = self.rt.music_json("music_browse", self.rt.base_args(), request).await?;
        Ok(serde_json::from_str::<EntryList>(&json)?.entries())
    }

    /// Downloads one track, then tags it.
    pub async fn download(
        &self,
        url: &str,
        opts: &DownloadOptions,
        progress: impl Fn(Progress) + Send + Sync + 'static,
        cancel: CancelToken,
    ) -> Result<Downloaded> {
        std::fs::create_dir_all(&opts.output_dir)?;
        let mut args = self.rt.base_args();
        args.extend([
            "--no-playlist".into(),
            "--continue".into(),
            "--windows-filenames".into(),
            "-f".into(),
            opts.format.clone(),
            "-P".into(),
            opts.output_dir.display().to_string(),
            "-o".into(),
            opts.template.clone(),
        ]);
        args.extend(opts.extra_args.iter().cloned());

        let json = self.rt.download_json(args, url.to_owned(), progress, cancel).await?;
        let info: Info = serde_json::from_str(&json)?;
        let path = info
            .requested_downloads
            .iter()
            .find_map(|d| d.filepath.clone())
            .ok_or_else(|| Error::Invalid("yt-dlp reported no output file".into()))?;
        // yt-dlp named the file from the upload's own title and artist.
        let named = info.untidied_track();
        let mut meta = named.clone().tidied();
        if meta.track_number.is_none() {
            meta.track_number = opts.track_number;
        }
        let path = if opts.template == DEFAULT_TEMPLATE { retitle_file(path, &named, &meta) } else { path };

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
        // YouTube audio is DASH-fragmented; yt-dlp only fixes that when it has ffmpeg.
        if matches!(ext, "m4a" | "mp4") {
            match remux::defragment_mp4(&path) {
                Ok(true) => tracing::info!(target: "ytmdl", "rewrote fragmented MP4 {}", path.display()),
                Ok(false) => {}
                Err(e) => tracing::warn!(target: "ytmdl", "could not defragment: {e}"),
            }
        }
        let mut tagged = false;
        if opts.write_tags && tag::can_tag(ext) {
            let cover = if opts.embed_cover { self.fetch_cover(&meta).await } else { None };
            let lyrics = if opts.lyrics { self.find_lyrics(&meta).await } else { None };
            tag::write_tags_and_lyrics(&path, &meta, cover, lyrics.as_deref())?;
            tagged = true;
        } else if opts.write_tags {
            tracing::warn!(target: "ytmdl", "not tagging {} (unsupported container)", path.display());
        }
        if tagged && opts.measure_loudness && loudness::can_measure(ext) {
            let file = path.clone();
            let measured = off_thread(move || loudness::measure(&file).and_then(|gain| loudness::write(&file, gain))).await;
            if let Err(e) = measured.and_then(|r| r) {
                tracing::warn!(target: "ytmdl", "no ReplayGain for {}: {e}", path.display());
            }
        }
        Ok(Downloaded { path, meta, tagged })
    }

    /// The lyrics to store with a download; a failed lookup only costs the lyrics.
    async fn find_lyrics(&self, meta: &TrackMeta) -> Option<String> {
        let song = lyrics::Song {
            title: &meta.title,
            artists: &meta.artists,
            album: meta.album.as_deref(),
            duration_secs: meta.duration_secs,
        };
        match self.lyrics(song).await {
            Ok(found) => found.and_then(|l| l.text().map(str::to_owned)),
            Err(e) => {
                tracing::warn!(target: "ytmdl", "lyrics for {}: {e}", meta.title);
                None
            }
        }
    }

    async fn fetch_cover(&self, meta: &TrackMeta) -> Option<tag::Cover> {
        for url in meta.cover_urls.iter().take(3) {
            match self.rt.fetch(url.clone(), MAX_COVER_BYTES).await {
                Ok(data) => match tag::Cover::sniff(data) {
                    Some(cover) => return Some(cover),
                    None => tracing::debug!(target: "ytmdl", "cover {url} is not JPEG/PNG"),
                },
                Err(e) => tracing::warn!(target: "ytmdl", "cover {url}: {e}"),
            }
        }
        None
    }
}

/// Renames a download named from its untidied title (see [`titles`]), when
/// it is laid out as [`DEFAULT_TEMPLATE`] has it.
fn retitle_file(path: PathBuf, named: &TrackMeta, meta: &TrackMeta) -> PathBuf {
    fn first(m: &TrackMeta) -> Option<&str> {
        m.artists.first().map(String::as_str)
    }
    let Some(to) = titles::retitled_path(&path, &meta.id, (&named.title, first(named)), (&meta.title, first(meta))) else {
        return path;
    };
    match titles::relocate(&path, &to) {
        Ok(()) => to,
        Err(e) => {
            tracing::warn!(target: "ytmdl", "could not rename {} to {}: {e}", path.display(), to.display());
            path
        }
    }
}

/// Runs `f` on its own thread, for file work too slow for the caller's
/// executor (the app awaits downloads on its UI thread).
async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new().name("ytmdl-file".into()).spawn(move || {
        let _ = tx.send(f());
    })?;
    rx.await.map_err(|_| Error::Stopped)
}

/// `application/x-www-form-urlencoded` encoding for a query string value.
pub(crate) fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_queries() {
        assert_eq!(super::encode_query("Daft Punk & co/é"), "Daft+Punk+%26+co%2F%C3%A9");
    }
}
