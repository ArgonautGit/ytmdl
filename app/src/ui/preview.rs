//! Renders every screen from the real components, with sample data, to static
//! HTML for checking the UI in a browser. `tools/ui-preview` runs it:
//!
//!   cargo test -p ytmdl-app --no-default-features ui::preview -- --ignored
//!
//! Output is target/ui-preview/index.html. The pages link app/assets/style.css,
//! so stylesheet edits show on reload without re-rendering. Sample data comes from
//! .deps/ui-sample.json (real search results; `tools/ui-preview --sample QUERY`)
//! when present, otherwise from built-in placeholders.

use std::path::PathBuf;
use std::sync::OnceLock;

use dioxus::prelude::*;
use ytmdl_core::{CancelToken, Entry, Progress, SearchSource};

use super::views::*;
use crate::jobs::{Job, JobState};

struct Sample {
    songs: Vec<Entry>,
    albums: Vec<Entry>,
    videos: Vec<Entry>,
    album_tracks: Vec<Entry>,
}

static SAMPLE: OnceLock<Sample> = OnceLock::new();

fn sample() -> &'static Sample {
    SAMPLE.get_or_init(|| {
        let path = workspace().join(".deps/ui-sample.json");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let v: serde_json::Value = serde_json::from_slice(&bytes).expect("ui-sample.json");
                let list = |key: &str| serde_json::from_value::<Vec<Entry>>(v[key].clone()).expect(key);
                Sample {
                    songs: list("songs"),
                    albums: list("albums"),
                    videos: list("videos"),
                    album_tracks: serde_json::from_value(v["album"]["entries"].clone()).expect("album"),
                }
            }
            Err(_) => placeholder_sample(),
        }
    })
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// (file name, caption, screen)
type Screen = (&'static str, &'static str, fn() -> Element);

const SCREENS: &[Screen] = &[
    ("library-songs", "Library, songs", library_songs),
    ("library-playing", "Library with the mini player", library_playing),
    ("now-playing", "Now playing", now_playing),
    ("library-albums", "Library, albums", library_albums),
    ("library-artists", "Library, artists", library_artists),
    ("library-empty", "Library, empty", library_empty),
    ("library-album", "Downloaded album", library_album),
    ("artist", "Artist", artist),
    ("startup", "Starting", startup),
    ("search-starting", "Search while the downloader starts", search_starting),
    ("search-idle", "Search, nothing typed", search_idle),
    ("search-songs", "Songs, mixed download states", search_songs),
    ("search-albums", "Albums", search_albums),
    ("search-videos", "Videos", search_videos),
    ("search-loading", "Searching", search_loading),
    ("album", "Album page", album),
    ("album-loading", "Album loading", album_loading),
    ("downloads", "Downloads", downloads),
    ("downloads-empty", "Downloads, empty", downloads_empty),
    ("settings", "Settings, no storage access", settings),
    ("startup-failed", "Startup failed", startup_failed),
];

#[test]
#[ignore = "writes target/ui-preview; run through tools/ui-preview"]
fn preview() {
    let out = workspace().join("target/ui-preview");
    std::fs::create_dir_all(&out).unwrap();
    let mut figures = String::new();
    for (name, caption, screen) in SCREENS {
        let mut dom = VirtualDom::new(*screen);
        dom.rebuild_in_place();
        let body = dioxus_ssr::render(&dom);
        let page = format!(
            r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">
<title>{caption}</title>
<link rel="stylesheet" href="../../app/assets/style.css">
</head>
<body>{body}</body>
</html>
"#
        );
        std::fs::write(out.join(format!("{name}.html")), page).unwrap();
        figures += &format!(
            r#"<figure><iframe src="{name}.html" loading="lazy"></iframe><figcaption><a href="{name}.html">{caption}</a></figcaption></figure>"#
        );
    }
    let index = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ytmdl screens</title>
<style>
body {{ margin: 0; background: #1b1b1d; color: #ddd; font: 14px system-ui, sans-serif; }}
.grid {{ display: flex; flex-wrap: wrap; gap: 28px; padding: 28px; }}
figure {{ margin: 0; }}
iframe {{ width: 412px; height: 892px; border: 0; border-radius: 28px; background: #000; box-shadow: 0 10px 30px #0009; }}
figcaption {{ margin: 10px 6px; }}
a {{ color: #aaa; }}
</style>
</head>
<body><div class="grid">{figures}</div></body>
</html>
"#
    );
    std::fs::write(out.join("index.html"), index).unwrap();
    println!("{}", out.join("index.html").display());
}

fn shell(tab: Tab, active: usize, page: Element) -> Element {
    rsx! {
        div { class: "app",
            Page { visible: true, {page} }
            BottomNav { tab, active, onselect: |_| {} }
        }
    }
}

fn search_page(source: SearchSource, query: &str, results: ResultsView, storage: bool) -> Element {
    rsx! {
        SearchPage {
            query: query.to_string(),
            source,
            results,
            storage,
            oninput: |_| {},
            onsubmit: |_| {},
            onclear: |_| {},
            onsource: |_| {},
            ondownload: |_| {},
            onopen: |_| {},
            onallow: |_| {},
        }
    }
}

fn song_items() -> Vec<SongItem> {
    let s = sample();
    s.songs
        .iter()
        .chain(&s.album_tracks)
        .enumerate()
        .map(|(i, e)| SongItem {
            id: i as i64,
            title: e.title.clone(),
            artists: e.artists.join(", "),
            album: e.album.clone(),
            duration_secs: e.duration_secs,
            art: e.thumbnail.clone(),
            playing: i == 1,
        })
        .collect()
}

fn album_items() -> Vec<AlbumItem> {
    sample()
        .albums
        .iter()
        .map(|e| AlbumItem { title: e.title.clone(), artist: e.artists.join(", "), year: e.year, art: e.thumbnail.clone() })
        .collect()
}

fn artist_items() -> Vec<ArtistItem> {
    let songs = &sample().songs;
    [("Daft Punk", 14, 3), ("Kevin MacLeod", 22, 4), ("Pharrell Williams", 1, 0), ("Nile Rodgers", 2, 1)]
        .iter()
        .enumerate()
        .map(|(i, (name, tracks, albums))| ArtistItem {
            name: name.to_string(),
            tracks: *tracks,
            albums: *albums,
            art: songs[i % songs.len()].thumbnail.clone(),
        })
        .collect()
}

fn library_page(view: LibraryView, songs: Vec<SongItem>) -> Element {
    shell(Tab::Library, 0, library_page_body(view, songs))
}

fn library_page_body(view: LibraryView, songs: Vec<SongItem>) -> Element {
    rsx! {
        LibraryPage {
            view,
            songs,
            albums: album_items(),
            artists: artist_items(),
            onview: |_| {},
            onplay: |_| {},
            onshuffle: |_| {},
            onalbum: |_| {},
            onartist: |_| {},
            onsearch: |_| {},
        }
    }
}

fn library_songs() -> Element {
    let songs = song_items().into_iter().map(|s| SongItem { playing: false, ..s }).collect();
    library_page(LibraryView::Songs, songs)
}

fn now_item() -> NowItem {
    let e = &sample().songs[1];
    NowItem {
        title: e.title.clone(),
        artists: e.artists.join(", "),
        album: e.album.clone(),
        art: e.thumbnail.clone(),
        art_large: e.thumbnail.as_deref().map(|u| ytmdl_core::art_url(u, 544)),
    }
}

fn library_playing() -> Element {
    rsx! {
        div { class: "app has-mini",
            Page { visible: true, {library_page_body(LibraryView::Songs, song_items())} }
            MiniPlayer { now: now_item(), progress: 0.37, playing: true, ontoggle: |_| {}, onnext: |_| {}, onopen: |_| {} }
            BottomNav { tab: Tab::Library, active: 0, onselect: |_| {} }
        }
    }
}

fn now_playing() -> Element {
    let queue = song_items().into_iter().take(8).collect();
    rsx! {
        div { class: "app has-mini",
            div { class: "overlay sheet",
                NowPlayingPage {
                    now: now_item(),
                    position: 51.0,
                    duration: 139.0,
                    playing: true,
                    buffering: false,
                    repeat: RepeatMode::All,
                    queue,
                    onclose: |_| {},
                    ontoggle: |_| {},
                    onnext: |_| {},
                    onprevious: |_| {},
                    onseeking: |_| {},
                    onseek: |_| {},
                    onrepeat: |_| {},
                    onskip: |_| {},
                }
            }
        }
    }
}

fn library_albums() -> Element {
    library_page(LibraryView::Albums, song_items())
}

fn library_artists() -> Element {
    library_page(LibraryView::Artists, song_items())
}

fn library_empty() -> Element {
    library_page(LibraryView::Songs, Vec::new())
}

fn library_album() -> Element {
    let album = &sample().albums[0];
    let mut header = AlbumHeader::from_entry(album);
    header.kind = None;
    header.cover = album.thumbnail.as_deref().map(|u| ytmdl_core::art_url(u, 544));
    let songs = song_items().into_iter().skip(sample().songs.len()).collect();
    shell(
        Tab::Library,
        0,
        rsx! {
            div { class: "overlay",
                LocalAlbumPage { header, songs, onback: |_| {}, onplay: |_| {}, onshuffle: |_| {} }
            }
        },
    )
}

fn artist() -> Element {
    let songs: Vec<SongItem> = song_items().into_iter().take(6).collect();
    let art = sample().songs[1].thumbnail.as_deref().map(|u| ytmdl_core::art_url(u, 544));
    shell(
        Tab::Library,
        0,
        rsx! {
            div { class: "overlay",
                ArtistPage {
                    name: "Kevin MacLeod".to_string(),
                    art,
                    songs,
                    albums: album_items(),
                    onback: |_| {},
                    onplay: |_| {},
                    onshuffle: |_| {},
                    onalbum: |_| {},
                }
            }
        },
    )
}

fn search_starting() -> Element {
    shell(Tab::Search, 0, search_page(SearchSource::MusicSongs, "", ResultsView::Starting, true))
}

fn startup() -> Element {
    rsx! { div { class: "app", StartupScreen {} } }
}

fn startup_failed() -> Element {
    let error = "unpacking the Python runtime from the APK: No space left on device (os error 28)".to_string();
    rsx! { div { class: "app", StartupScreen { error } } }
}

fn search_idle() -> Element {
    shell(Tab::Search, 0, search_page(SearchSource::MusicSongs, "", ResultsView::Idle, false))
}

fn search_loading() -> Element {
    shell(Tab::Search, 0, search_page(SearchSource::MusicSongs, "kevin macleod", ResultsView::Loading, true))
}

fn states_for(entries: &[Entry]) -> Vec<(Entry, TrackState)> {
    let cycle = [
        TrackState::Done,
        TrackState::Downloading(0.42),
        TrackState::Queued,
        TrackState::Failed,
    ];
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.clone(), cycle.get(i).copied().unwrap_or_default()))
        .collect()
}

fn search_songs() -> Element {
    let rows = states_for(&sample().songs);
    shell(Tab::Search, 2, search_page(SearchSource::MusicSongs, "kevin macleod", ResultsView::Songs(rows), true))
}

fn search_albums() -> Element {
    let rows = sample().albums.clone();
    shell(Tab::Search, 0, search_page(SearchSource::MusicAlbums, "kevin macleod", ResultsView::Albums(rows), true))
}

fn search_videos() -> Element {
    let rows = states_for(&sample().videos[..sample().videos.len().min(2)])
        .into_iter()
        .chain(sample().videos.iter().skip(2).map(|e| (e.clone(), TrackState::Idle)))
        .collect();
    shell(Tab::Search, 1, search_page(SearchSource::YouTube, "kevin macleod", ResultsView::Videos(rows), true))
}

fn album_page(tracks: AlbumTracks) -> Element {
    let header = AlbumHeader::from_entry(&sample().albums[0]);
    shell(
        Tab::Search,
        2,
        rsx! {
            div { class: "overlay",
                AlbumPage {
                    header,
                    tracks,
                    onback: |_| {},
                    ondownload: |_| {},
                    ondownloadall: |_| {},
                    onretry: |_| {},
                }
            }
        },
    )
}

fn album() -> Element {
    let rows = sample()
        .album_tracks
        .iter()
        .enumerate()
        .map(|(i, e)| (e.clone(), if i < 2 { TrackState::Done } else { TrackState::Idle }))
        .collect();
    album_page(AlbumTracks::Loaded(rows))
}

fn album_loading() -> Element {
    album_page(AlbumTracks::Loading)
}

fn job(id: u64, entry: &Entry, state: JobState, progress: Option<Progress>) -> Job {
    Job { id, entry: entry.clone(), state, progress, cancel: CancelToken::new() }
}

fn downloads() -> Element {
    let s = &sample().songs;
    let done = |i: usize| JobState::Done { path: format!("/storage/emulated/0/Music/x/y/{i}.m4a").into(), tagged: true };
    let jobs = vec![
        job(1, &s[4 % s.len()], done(1), None),
        job(2, &s[5 % s.len()], JobState::Failed("HTTP Error 403: Forbidden".into()), None),
        job(3, &s[6 % s.len()], JobState::Cancelled, None),
        job(4, &s[0], done(4), None),
        job(
            5,
            &s[1],
            JobState::Downloading,
            Some(Progress {
                status: "downloading".into(),
                downloaded_bytes: Some(2_140_000),
                total_bytes: Some(5_310_000),
                speed: Some(1_250_000.0),
                eta: Some(3.0),
                ..Default::default()
            }),
        ),
        job(6, &s[2], JobState::Queued, None),
    ];
    shell(
        Tab::Downloads,
        2,
        rsx! {
            DownloadsPage {
                jobs,
                storage: true,
                oncancel: |_| {},
                oncancelall: |_| {},
                onretry: |_| {},
                onclear: |_| {},
                onallow: |_| {},
            }
        },
    )
}

fn downloads_empty() -> Element {
    shell(
        Tab::Downloads,
        0,
        rsx! {
            DownloadsPage {
                jobs: Vec::new(),
                storage: true,
                oncancel: |_| {},
                oncancelall: |_| {},
                onretry: |_| {},
                onclear: |_| {},
                onallow: |_| {},
            }
        },
    )
}

fn settings() -> Element {
    let about = About {
        app: env!("CARGO_PKG_VERSION").into(),
        yt_dlp: "2026.08.19".into(),
        python: "3.14.7 (android)".into(),
        openssl: "OpenSSL 3.5.7 9 Jun 2026".into(),
    };
    shell(
        Tab::Settings,
        0,
        rsx! {
            SettingsPage {
                about,
                output: "/storage/emulated/0/Android/data/dev.nick.ytmdl/files/Music".to_string(),
                storage: false,
                update: Some("yt-dlp 2026.08.19 is the latest.".to_string()),
                checking: false,
                onupdate: |_| {},
                onallow: |_| {},
            }
        },
    )
}

// ---- placeholder data (no .deps/ui-sample.json) ----

/// A gradient square as a data: URI, standing in for cover art.
fn art(hue: u32) -> String {
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 10 10'><defs><linearGradient id='g' x2='1' y2='1'>\
         <stop offset='0' stop-color='hsl({hue},65%,55%)'/><stop offset='1' stop-color='hsl({},55%,22%)'/>\
         </linearGradient></defs><rect width='10' height='10' fill='url(#g)'/></svg>",
        (hue + 50) % 360
    );
    let encoded: String = svg
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("data:image/svg+xml,{encoded}")
}

fn entry(i: usize, title: &str, album: Option<&str>, secs: f64, kind: &str, year: Option<i32>) -> Entry {
    Entry {
        id: format!("id{i}"),
        url: format!("https://music.youtube.com/watch?v=id{i}"),
        title: title.into(),
        artists: vec!["Kevin MacLeod".into()],
        album: album.map(Into::into),
        duration_secs: Some(secs),
        thumbnail: Some(art(i as u32 * 47 % 360)),
        kind: Some(kind.into()),
        year,
        track_number: None,
    }
}

fn placeholder_sample() -> Sample {
    let songs = [
        ("Monkeys Spinning Monkeys", "Monkeys Spinning Monkeys", 125.0),
        ("Sneaky Snitch", "Comedy Scoring", 139.0),
        ("Fluffing a Duck", "Comedy Scoring", 81.0),
        ("Carefree", "Carefree", 211.0),
        ("Local Forecast - Elevator", "Local Forecast", 184.0),
        ("Wallpaper", "Wallpaper", 205.0),
        ("Investigations", "Film Noir", 156.0),
        ("Pixel Peeker Polka - faster", "Pixel Peeker Polka", 211.0),
    ];
    let albums = [("Action Cuts", 2008), ("Comedy Scoring", 2009), ("Film Noir", 2010), ("Calmant", 2012)];
    Sample {
        songs: songs.iter().enumerate().map(|(i, (t, a, s))| entry(i, t, Some(a), *s, "song", None)).collect(),
        albums: albums.iter().enumerate().map(|(i, (t, y))| entry(20 + i, t, None, 0.0, "album", Some(*y))).collect(),
        videos: songs.iter().take(4).enumerate().map(|(i, (t, _, s))| entry(40 + i, &format!("{t} (Official)"), None, *s, "video", None)).collect(),
        album_tracks: ["A Mission", "Action", "All This", "Americana", "Anguish", "Arcadia", "Awkward Meeting", "Backbay Lounge"]
            .iter()
            .enumerate()
            .map(|(i, t)| entry(60 + i, t, Some("Action Cuts"), 60.0 + i as f64 * 23.0, "song", None))
            .collect(),
    }
}
