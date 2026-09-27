//! Search and download music from YouTube / YouTube Music.
//!
//! Extraction is delegated to an embedded upstream yt-dlp (see [`runtime`]); this
//! crate adds a typed API, download progress/cancellation, and tagging.

pub mod error;
pub mod model;
pub mod remux;
pub mod runtime;
pub mod selftest;
pub mod tag;

use std::path::PathBuf;

pub use error::{Error, Result};
pub use model::{CollectionKind, Downloaded, Entry, Resolved, TrackMeta, art_url};
pub use runtime::{CancelToken, Channel, Progress, Runtime, RuntimeConfig, UpdateOutcome, VersionInfo};

use model::Info;

/// Largest cover image we will embed.
const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;

/// Prefer AAC in MP4 (taggable without ffmpeg), then any audio-only stream.
pub const DEFAULT_FORMAT: &str = "bestaudio[ext=m4a]/bestaudio";

/// `<out>/<Artist>/<Album>/<Title> [<id>].<ext>`
pub const DEFAULT_TEMPLATE: &str =
    "%(album_artist,artists.0,creators.0,uploader|Unknown Artist)s/%(album|Singles)s/%(track,title)s [%(id)s].%(ext)s";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    /// YouTube Music "Songs" results.
    MusicSongs,
    /// YouTube Music "Albums" results.
    MusicAlbums,
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
        };
        let mut args = self.rt.base_args();
        args.extend(["--flat-playlist".into(), "--playlist-end".into(), limit.to_string()]);
        let info: Info = serde_json::from_str(&self.rt.extract_json(args, url).await?)?;
        Ok(info.entries.iter().filter_map(Info::to_entry).collect())
    }

    /// Track metadata, or the members of an album/playlist.
    pub async fn resolve(&self, url: &str) -> Result<Resolved> {
        let mut args = self.rt.base_args();
        args.push("--flat-playlist".into());
        let info: Info = serde_json::from_str(&self.rt.extract_json(args, url.to_owned()).await?)?;
        if info.kind.as_deref() == Some("playlist") {
            let kind = if url.contains("OLAK5uy_") || url.contains("/browse/MPREb") {
                CollectionKind::Album
            } else {
                CollectionKind::Playlist
            };
            let mut title = info.title.clone().unwrap_or_default();
            let mut entries: Vec<Entry> = info.entries.iter().filter_map(Info::to_entry).collect();
            if kind == CollectionKind::Album {
                title = model::album_title(&title);
                for e in &mut entries {
                    e.album.get_or_insert_with(|| title.clone());
                }
            }
            return Ok(Resolved::Collection { title, kind, entries });
        }
        Ok(Resolved::Track(info.to_track()))
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
        let meta = info.to_track();

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
            tag::write_tags(&path, &meta, cover)?;
            tagged = true;
        } else if opts.write_tags {
            tracing::warn!(target: "ytmdl", "not tagging {} (unsupported container)", path.display());
        }
        Ok(Downloaded { path, meta, tagged })
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

/// `application/x-www-form-urlencoded` encoding for a query string value.
fn encode_query(s: &str) -> String {
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
