//! Presentational pieces: plain props in, events out. They hold no app state, so
//! `preview` can render every screen with sample data.

use dioxus::prelude::*;
use ytmdl_core::{Channel, Entry, SearchSource, art_url};

use super::icons::{Icon, Svg};
use crate::jobs::{Job, JobState};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Library,
    Search,
    Downloads,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LibraryView {
    Songs,
    Albums,
    Artists,
    Playlists,
}

/// A downloaded track as a list row.
#[derive(Clone, PartialEq, Debug)]
pub struct SongItem {
    /// Unique in its list (the same song can be in a queue twice).
    pub key: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    /// Small cover URL.
    pub art: Option<String>,
    pub playing: bool,
    /// Where the song is in the queue's A-B loop, if it is.
    pub mark: Option<LoopMark>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LoopMark {
    /// Where the loop starts again.
    A,
    Inside,
    /// Where the loop ends.
    B,
    /// A one-song loop.
    AB,
}

/// The queue's A-B loop, as the queue header shows it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum QueueLoopView {
    Off,
    /// Waiting for the first song to be tapped.
    PickA,
    /// Waiting for the last song.
    PickB,
    /// Songs `first..=last` (1-based queue positions) repeat.
    Looping { first: usize, last: usize },
}

#[derive(Clone, PartialEq, Debug)]
pub struct PlaylistItem {
    pub name: String,
    pub tracks: u32,
    pub duration_secs: f64,
    pub art: Option<String>,
    /// Follows a YouTube playlist.
    pub synced: bool,
}

/// A synced playlist's state, under its title.
#[derive(Clone, PartialEq, Debug)]
pub enum SyncView {
    Syncing,
    Failed(String),
    /// `ago`: when it last synced; `pending`: songs still to download.
    Synced { ago: String, pending: u32 },
}

/// A row in a menu sheet.
#[derive(Clone, PartialEq, Debug)]
pub struct MenuItem {
    pub icon: Icon,
    pub label: String,
    pub sub: Option<String>,
    /// Destructive (shown in red).
    pub danger: bool,
}

impl MenuItem {
    pub fn new(icon: Icon, label: impl Into<String>) -> Self {
        MenuItem { icon, label: label.into(), sub: None, danger: false }
    }
}

/// What a menu sheet is about, shown above its items.
#[derive(Clone, PartialEq, Debug)]
pub struct MenuHead {
    pub title: String,
    pub sub: String,
    pub art: Option<String>,
    pub icon: Icon,
}

#[derive(Clone, PartialEq, Debug)]
pub struct AlbumItem {
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    pub art: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

/// The song the player is on.
#[derive(Clone, PartialEq, Debug)]
pub struct NowItem {
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    /// Small cover URL (mini player).
    pub art: Option<String>,
    /// Large cover URL (now playing).
    pub art_large: Option<String>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ArtistItem {
    pub name: String,
    pub tracks: u32,
    pub albums: u32,
    pub art: Option<String>,
}

/// Download state of one search result or album track.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum TrackState {
    #[default]
    Idle,
    Queued,
    Downloading(f64),
    Done,
    Failed,
}

impl TrackState {
    /// Whether "download" should start (or restart) it.
    pub fn wants_download(self) -> bool {
        matches!(self, TrackState::Idle | TrackState::Failed)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum ResultsView {
    /// The downloader is still starting.
    Starting,
    /// The downloader failed to start.
    Unavailable(String),
    Idle,
    Loading,
    Error(String),
    Songs(Vec<(Entry, TrackState)>),
    Videos(Vec<(Entry, TrackState)>),
    Albums(Vec<Entry>),
}

#[derive(Clone, PartialEq, Debug)]
pub struct AlbumHeader {
    pub title: String,
    pub artists: Vec<String>,
    pub year: Option<i32>,
    pub kind: Option<String>,
    pub cover: Option<String>,
}

impl AlbumHeader {
    /// "Album • Artist • 2008"
    fn meta(&self) -> String {
        let kind = kind_label(self.kind.as_deref()).or(Some("Album".into()));
        dotted([kind, Some(self.artists.join(", ")), self.year.map(|y| y.to_string())])
    }

    pub fn from_entry(e: &Entry) -> Self {
        AlbumHeader {
            title: e.title.clone(),
            artists: e.artists.clone(),
            year: e.year,
            kind: e.kind.clone(),
            cover: e.thumbnail.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum AlbumTracks {
    Loading,
    Error(String),
    Loaded(Vec<(Entry, TrackState)>),
}

#[derive(Clone, PartialEq, Debug)]
pub struct About {
    pub app: String,
    pub yt_dlp: String,
    pub python: String,
    pub openssl: String,
}

pub fn duration_text(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn total_text(secs: f64) -> String {
    let m = (secs / 60.0).round() as u64;
    match m {
        0 => "under a minute".into(),
        1..60 => format!("{m} min"),
        _ => format!("{} hr {} min", m / 60, m % 60),
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1e6)
}

/// "a • b • c", skipping missing and empty parts.
fn dotted<const N: usize>(parts: [Option<String>; N]) -> String {
    parts.into_iter().flatten().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" • ")
}

fn kind_label(kind: Option<&str>) -> Option<String> {
    Some(match kind? {
        "ep" => "EP".into(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
    })
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ---- shell ----

#[component]
pub fn Page(visible: bool, children: Element) -> Element {
    rsx! {
        div { class: if visible { "page" } else { "page inactive" }, {children} }
    }
}

#[component]
pub fn BottomNav(tab: Tab, active: usize, onselect: EventHandler<Tab>) -> Element {
    let item = move |t: Tab, icon: Icon, label: &'static str, count: usize| {
        rsx! {
            button {
                class: if tab == t { "navitem selected" } else { "navitem" },
                onclick: move |_| onselect.call(t),
                Svg { icon }
                span { "{label}" }
                if count > 0 {
                    span { class: "count", "{count}" }
                }
            }
        }
    };
    rsx! {
        nav { class: "bottomnav",
            {item(Tab::Library, Icon::Library, "Library", 0)}
            {item(Tab::Search, Icon::Search, "Search", 0)}
            {item(Tab::Downloads, Icon::Download, "Downloads", active)}
            {item(Tab::Settings, Icon::Settings, "Settings", 0)}
        }
    }
}

/// Keyed by the caller so each new message restarts the fade animation.
#[component]
pub fn Toast(message: String) -> Element {
    rsx! {
        div { class: "toast", role: "status", "{message}" }
    }
}

#[component]
pub fn StartupScreen(error: Option<String>) -> Element {
    rsx! {
        div { class: "startup",
            div { class: "logo", Svg { icon: Icon::Download, size: 36 } }
            div { class: "brand", "ytmdl" }
            if let Some(error) = error {
                div { class: "error-card",
                    h3 { "Could not start" }
                    pre { "{error}" }
                }
            } else {
                div { class: "spinner" }
            }
        }
    }
}

#[component]
pub fn StorageBanner(onallow: EventHandler<()>) -> Element {
    rsx! {
        div { class: "banner",
            Svg { icon: Icon::Folder }
            span { "Allow access to all files to save into your Music folder, where music apps find it." }
            button { class: "primary small", onclick: move |_| onallow.call(()), "Allow" }
        }
    }
}

#[component]
pub fn EmptyState(icon: Icon, title: String, text: String, action: Option<Element>) -> Element {
    rsx! {
        div { class: "empty",
            Svg { icon, size: 48 }
            h2 { "{title}" }
            p { "{text}" }
            {action}
        }
    }
}

// ---- rows ----

#[component]
pub fn Cover(
    url: Option<String>,
    #[props(default = "cover".to_string())] class: String,
    #[props(default = Icon::Music)] icon: Icon,
) -> Element {
    rsx! {
        div { class: "{class}",
            Svg { icon, size: 20 }
            if let Some(url) = url {
                Img { url }
            }
        }
    }
}

/// Remote art. Google's image hosts answer requests that carry a Referer with 429,
/// so none is sent; a failed image removes itself and leaves the placeholder.
#[component]
fn Img(url: String, #[props(default)] class: Option<String>) -> Element {
    rsx! {
        img {
            class,
            src: "{url}",
            loading: "lazy",
            alt: "",
            referrerpolicy: "no-referrer",
            "onerror": "this.remove()",
        }
    }
}

#[component]
fn Ring(fraction: f64) -> Element {
    let circumference = 2.0 * std::f64::consts::PI * 9.0;
    let dash = format!("{:.2} {:.2}", circumference * fraction.clamp(0.0, 1.0), circumference);
    rsx! {
        svg { class: "ring", width: "24", height: "24", view_box: "0 0 24 24",
            circle { class: "ring-track", cx: "12", cy: "12", r: "9" }
            circle {
                class: "ring-fill",
                cx: "12",
                cy: "12",
                r: "9",
                stroke_dasharray: "{dash}",
                transform: "rotate(-90 12 12)",
            }
        }
    }
}

/// The trailing control of a track row.
#[component]
fn TrackAction(state: TrackState, ondownload: EventHandler<()>) -> Element {
    match state {
        TrackState::Idle => rsx! {
            button { class: "icon-btn", "aria-label": "Download", onclick: move |_| ondownload.call(()),
                Svg { icon: Icon::DownloadCircle }
            }
        },
        TrackState::Queued => rsx! {
            div { class: "icon-btn", "aria-label": "Queued", div { class: "spinner small" } }
        },
        TrackState::Downloading(f) => rsx! {
            div { class: "icon-btn", "aria-label": "Downloading", Ring { fraction: f } }
        },
        TrackState::Done => rsx! {
            div { class: "icon-btn", "aria-label": "Downloaded",
                span { class: "done-badge", Svg { icon: Icon::Check, size: 16 } }
            }
        },
        TrackState::Failed => rsx! {
            button { class: "icon-btn failed", "aria-label": "Retry", onclick: move |_| ondownload.call(()),
                Svg { icon: Icon::Retry }
            }
        },
    }
}

#[component]
pub fn SongRow(entry: Entry, state: TrackState, ondownload: EventHandler<()>) -> Element {
    let album = entry.album.clone().filter(|a| *a != entry.title);
    let sub = dotted([Some(entry.artists.join(", ")), album, entry.duration_secs.map(duration_text)]);
    rsx! {
        li { class: "row",
            Cover { url: entry.thumbnail.clone() }
            div { class: "meta",
                div { class: "title", "{entry.title}" }
                div { class: "sub", "{sub}" }
            }
            TrackAction { state, ondownload }
        }
    }
}

#[component]
pub fn VideoRow(entry: Entry, state: TrackState, ondownload: EventHandler<()>) -> Element {
    let artists = entry.artists.join(", ");
    rsx! {
        li { class: "row",
            div { class: "thumb",
                Cover { url: entry.thumbnail.clone(), class: "cover wide" }
                if let Some(d) = entry.duration_secs {
                    span { class: "badge", {duration_text(d)} }
                }
            }
            div { class: "meta",
                div { class: "title two-lines", "{entry.title}" }
                div { class: "sub", "{artists}" }
            }
            TrackAction { state, ondownload }
        }
    }
}

#[component]
pub fn AlbumRow(entry: Entry, onopen: EventHandler<()>) -> Element {
    let sub = dotted([
        kind_label(entry.kind.as_deref()),
        Some(entry.artists.join(", ")),
        entry.year.map(|y| y.to_string()),
    ]);
    rsx! {
        li { class: "row tappable", onclick: move |_| onopen.call(()),
            Cover { url: entry.thumbnail.clone() }
            div { class: "meta",
                div { class: "title", "{entry.title}" }
                div { class: "sub", "{sub}" }
            }
            span { class: "chevron", Svg { icon: Icon::ChevronRight, size: 20 } }
        }
    }
}

#[component]
fn SkeletonRows(count: usize) -> Element {
    rsx! {
        ul { class: "list skeleton", "aria-busy": "true",
            for i in 0..count {
                li { key: "{i}", class: "row",
                    div { class: "cover" }
                    div { class: "meta",
                        div { class: "line" }
                        div { class: "line short" }
                    }
                }
            }
        }
    }
}

// ---- search ----

#[component]
pub fn SearchPage(
    query: String,
    source: SearchSource,
    results: ResultsView,
    storage: bool,
    oninput: EventHandler<String>,
    onsubmit: EventHandler<()>,
    onclear: EventHandler<()>,
    onsource: EventHandler<SearchSource>,
    ondownload: EventHandler<Entry>,
    onopen: EventHandler<Entry>,
    onallow: EventHandler<()>,
) -> Element {
    let sources = [("Songs", SearchSource::MusicSongs), ("Albums", SearchSource::MusicAlbums), ("Videos", SearchSource::YouTube)];
    let has_query = !query.is_empty();
    rsx! {
        header { class: "topbar",
            form {
                class: "searchbar",
                onsubmit: move |e| {
                    e.prevent_default();
                    onsubmit.call(());
                },
                Svg { icon: Icon::Search, size: 20 }
                input {
                    r#type: "search",
                    "enterkeyhint": "search",
                    placeholder: "Search songs, albums, or paste a link",
                    value: "{query}",
                    oninput: move |e| oninput.call(e.value()),
                }
                if has_query {
                    button { r#type: "button", class: "icon-btn small", "aria-label": "Clear", onclick: move |_| onclear.call(()),
                        Svg { icon: Icon::Close, size: 20 }
                    }
                }
            }
            div { class: "chips",
                for (label , value) in sources {
                    button {
                        key: "{label}",
                        class: if source == value { "chip selected" } else { "chip" },
                        onclick: move |_| onsource.call(value),
                        "{label}"
                    }
                }
            }
        }
        if !storage {
            StorageBanner { onallow }
        }
        match results {
            ResultsView::Starting => rsx! {
                div { class: "empty",
                    div { class: "spinner" }
                    h2 { "Getting ready" }
                    p { "Starting the downloader. Your library already works." }
                }
            },
            ResultsView::Unavailable(e) => rsx! {
                EmptyState { icon: Icon::Alert, title: "The downloader could not start", text: e }
            },
            ResultsView::Idle => rsx! {
                EmptyState {
                    icon: Icon::Search,
                    title: "Find music to download",
                    text: "Search YouTube Music, or paste a link to a song, album or playlist.",
                }
            },
            ResultsView::Loading => rsx! { SkeletonRows { count: 8 } },
            ResultsView::Error(e) => rsx! {
                EmptyState {
                    icon: Icon::Alert,
                    title: "Search failed",
                    text: e,
                    action: rsx! { button { class: "secondary", onclick: move |_| onsubmit.call(()), "Try again" } },
                }
            },
            ResultsView::Songs(rows) | ResultsView::Videos(rows) if rows.is_empty() => rsx! { NoResults {} },
            ResultsView::Albums(rows) if rows.is_empty() => rsx! { NoResults {} },
            ResultsView::Songs(rows) => rsx! {
                ul { class: "list",
                    for (entry , state) in rows {
                        SongRow { key: "{entry.id}", entry: entry.clone(), state, ondownload: move |_| ondownload.call(entry.clone()) }
                    }
                }
            },
            ResultsView::Videos(rows) => rsx! {
                ul { class: "list",
                    for (entry , state) in rows {
                        VideoRow { key: "{entry.id}", entry: entry.clone(), state, ondownload: move |_| ondownload.call(entry.clone()) }
                    }
                }
            },
            ResultsView::Albums(rows) => rsx! {
                ul { class: "list",
                    for entry in rows {
                        AlbumRow { key: "{entry.id}", entry: entry.clone(), onopen: move |_| onopen.call(entry.clone()) }
                    }
                }
            },
        }
    }
}

#[component]
fn NoResults() -> Element {
    rsx! {
        EmptyState { icon: Icon::Search, title: "No results", text: "Try other words, or another tab above." }
    }
}

// ---- album ----

/// An album or playlist from YouTube. `saved` is set for playlists: whether
/// it is in the library as a synced playlist.
#[component]
pub fn AlbumPage(
    header: AlbumHeader,
    tracks: AlbumTracks,
    #[props(default)] saved: Option<bool>,
    onback: EventHandler<()>,
    ondownload: EventHandler<Entry>,
    /// Downloads every song (and saves a playlist as a synced playlist).
    ondownloadall: EventHandler<()>,
    /// Opens the saved playlist.
    #[props(default)]
    onopensaved: EventHandler<()>,
    onretry: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "album",
            BackButton { onback }
            Hero { cover: header.cover.as_deref().map(|u| art_url(u, 544)), title: header.title.clone(), meta: header.meta(),
                match &tracks {
                    AlbumTracks::Loaded(rows) => rsx! { AlbumSummary { rows: rows.clone(), saved, ondownloadall, onopensaved } },
                    _ => rsx! {},
                }
            }
            match tracks {
                AlbumTracks::Loading => rsx! { SkeletonRows { count: 6 } },
                AlbumTracks::Error(e) => rsx! {
                    EmptyState {
                        icon: Icon::Alert,
                        title: "Could not load the album",
                        text: e,
                        action: rsx! { button { class: "secondary", onclick: move |_| onretry.call(()), "Try again" } },
                    }
                },
                AlbumTracks::Loaded(rows) => rsx! {
                    ol { class: "list tracks",
                        for (i , (entry , state)) in rows.into_iter().enumerate() {
                            li { key: "{entry.id}", class: "row",
                                span { class: "index", "{i + 1}" }
                                div { class: "meta",
                                    div { class: "title", "{entry.title}" }
                                    div { class: "sub",
                                        {dotted([Some(entry.artists.join(", ")), entry.duration_secs.map(duration_text)])}
                                    }
                                }
                                TrackAction { state, ondownload: move |_| ondownload.call(entry.clone()) }
                            }
                        }
                    }
                },
            }
        }
    }
}

#[component]
pub fn BackButton(onback: EventHandler<()>) -> Element {
    rsx! {
        button { class: "icon-btn back", "aria-label": "Back", onclick: move |_| onback.call(()),
            Svg { icon: Icon::Back }
        }
    }
}

/// Big cover over a blurred copy of itself, then the title and `children`.
#[component]
fn Hero(
    cover: Option<String>,
    title: String,
    meta: String,
    #[props(default)] round: bool,
    #[props(default = Icon::Music)] icon: Icon,
    children: Element,
) -> Element {
    rsx! {
        div { class: "album-head",
            div { class: "hero-wrap",
                if let Some(url) = cover.clone() {
                    Img { url, class: "hero-bg" }
                }
                div { class: "hero-fade" }
            }
            div { class: "hero",
                Cover { url: cover, class: if round { "cover hero-cover round" } else { "cover hero-cover" }, icon }
                h1 { "{title}" }
                div { class: "sub", "{meta}" }
                {children}
            }
        }
    }
}

#[component]
fn AlbumSummary(
    rows: Vec<(Entry, TrackState)>,
    saved: Option<bool>,
    ondownloadall: EventHandler<()>,
    onopensaved: EventHandler<()>,
) -> Element {
    let total: f64 = rows.iter().filter_map(|(e, _)| e.duration_secs).sum();
    let done = rows.iter().filter(|(_, s)| *s == TrackState::Done).count();
    let todo = rows.iter().filter(|(_, s)| s.wants_download()).count();
    let busy = rows.len() - done - todo;
    let line = dotted([Some(plural(rows.len(), "song", "songs")), (total > 0.0).then(|| total_text(total))]);
    rsx! {
        div { class: "sub", "{line}" }
        if saved == Some(false) {
            div { class: "sub", "Saves it to your playlists, kept in sync with YouTube" }
        }
        if saved == Some(false) && todo == 0 {
            button { class: "primary", onclick: move |_| ondownloadall.call(()),
                Svg { icon: Icon::Plus, size: 20 }
                "Add to library"
            }
        } else if saved == Some(true) {
            div { class: "hero-actions",
                if todo > 0 {
                    button { class: "primary", onclick: move |_| ondownloadall.call(()),
                        Svg { icon: Icon::Download, size: 20 }
                        "Download {todo} more"
                    }
                }
                button { class: "secondary", onclick: move |_| onopensaved.call(()),
                    Svg { icon: Icon::Playlist, size: 20 }
                    "Open playlist"
                }
            }
        } else if todo > 0 {
            button { class: "primary", onclick: move |_| ondownloadall.call(()),
                Svg { icon: Icon::Download, size: 20 }
                if done + busy == 0 { "Download all" } else { "Download {todo} more" }
            }
        } else if busy > 0 {
            div { class: "primary ghost", div { class: "spinner small" } "Downloading {busy}" }
        } else if done > 0 {
            div { class: "primary ghost", Svg { icon: Icon::Check, size: 20 } "Downloaded" }
        }
    }
}

// ---- library ----

#[component]
pub fn LibraryPage(
    view: LibraryView,
    songs: Vec<SongItem>,
    albums: Vec<AlbumItem>,
    artists: Vec<ArtistItem>,
    playlists: Vec<PlaylistItem>,
    onview: EventHandler<LibraryView>,
    onplay: EventHandler<usize>,
    /// A song's menu.
    onmore: EventHandler<usize>,
    onshuffle: EventHandler<()>,
    onalbum: EventHandler<usize>,
    onartist: EventHandler<usize>,
    onplaylist: EventHandler<usize>,
    onplaylistmore: EventHandler<usize>,
    onnewplaylist: EventHandler<()>,
    onsearch: EventHandler<()>,
) -> Element {
    let views = [
        ("Songs", LibraryView::Songs),
        ("Albums", LibraryView::Albums),
        ("Artists", LibraryView::Artists),
        ("Playlists", LibraryView::Playlists),
    ];
    rsx! {
        header { class: "topbar",
            h1 { "Library" }
            div { class: "chips",
                for (label , value) in views {
                    button {
                        key: "{label}",
                        class: if view == value { "chip selected" } else { "chip" },
                        onclick: move |_| onview.call(value),
                        "{label}"
                    }
                }
            }
        }
        if songs.is_empty() {
            EmptyState {
                icon: Icon::Library,
                title: "Your library is empty",
                text: "Songs you download show up here, ready to play offline.",
                action: rsx! {
                    button { class: "primary", onclick: move |_| onsearch.call(()), Svg { icon: Icon::Search, size: 20 } "Find music" }
                },
            }
        } else {
            match view {
                LibraryView::Songs => rsx! {
                    div { class: "list-head",
                        span { class: "section-note", {plural(songs.len(), "song", "songs")} }
                        button { class: "secondary small", onclick: move |_| onshuffle.call(()),
                            Svg { icon: Icon::Shuffle, size: 18 }
                            "Shuffle"
                        }
                    }
                    SongList { songs, onplay, onmore }
                },
                LibraryView::Albums if albums.is_empty() => rsx! {
                    EmptyState { icon: Icon::Disc, title: "No albums yet", text: "Download a whole album, or songs that belong to one." }
                },
                LibraryView::Albums => rsx! { AlbumGrid { albums, onopen: onalbum } },
                LibraryView::Artists => rsx! {
                    ul { class: "list",
                        for (i , artist) in artists.into_iter().enumerate() {
                            ArtistRow { key: "{artist.name}", artist, onopen: move |_| onartist.call(i) }
                        }
                    }
                },
                LibraryView::Playlists => rsx! {
                    ul { class: "list",
                        li { class: "row tappable", onclick: move |_| onnewplaylist.call(()),
                            div { class: "cover new", Svg { icon: Icon::Plus } }
                            div { class: "meta", div { class: "title", "New playlist" } }
                        }
                        for (i , playlist) in playlists.into_iter().enumerate() {
                            PlaylistRow {
                                key: "{i}/{playlist.name}",
                                playlist,
                                onopen: move |_| onplaylist.call(i),
                                onmore: move |_| onplaylistmore.call(i),
                            }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn PlaylistRow(playlist: PlaylistItem, onopen: EventHandler<()>, onmore: EventHandler<()>) -> Element {
    let kind = if playlist.synced { "Synced" } else { "Playlist" };
    rsx! {
        li { class: "row tappable song", onclick: move |_| onopen.call(()),
            Cover { url: playlist.art.clone(), icon: Icon::Playlist }
            div { class: "meta",
                div { class: "title", "{playlist.name}" }
                div { class: "sub",
                    if playlist.synced {
                        Svg { icon: Icon::Sync, size: 13 }
                    }
                    {dotted([Some(kind.into()), Some(plural(playlist.tracks as usize, "song", "songs"))])}
                }
            }
            MoreButton { onmore }
        }
    }
}

/// ⋮, opening a row's menu without triggering the row.
#[component]
fn MoreButton(onmore: EventHandler<()>) -> Element {
    rsx! {
        button {
            class: "icon-btn more",
            "aria-label": "More",
            onclick: move |e| {
                e.stop_propagation();
                onmore.call(());
            },
            Svg { icon: Icon::More, size: 20 }
        }
    }
}

/// Downloaded songs; `numbered` shows album positions instead of covers, and
/// `onmore` adds each song's menu button.
#[component]
pub fn SongList(
    songs: Vec<SongItem>,
    onplay: EventHandler<usize>,
    #[props(default)] onmore: Option<EventHandler<usize>>,
    #[props(default)] numbered: bool,
) -> Element {
    rsx! {
        ul { class: if numbered { "list tracks" } else { "list" },
            for (i , song) in songs.into_iter().enumerate() {
                SongItemRow {
                    key: "{song.key}",
                    song,
                    index: numbered.then_some(i + 1),
                    onplay: move |_| onplay.call(i),
                    onmore: onmore.map(|m| EventHandler::new(move |_| m.call(i))),
                }
            }
        }
    }
}

#[component]
fn SongItemRow(song: SongItem, index: Option<usize>, onplay: EventHandler<()>, onmore: Option<EventHandler<()>>) -> Element {
    let duration = song.duration_secs.map(duration_text);
    let sub = match index {
        Some(_) => dotted([Some(song.artists.clone()), duration]),
        None => dotted([Some(song.artists.clone()), song.album.clone().filter(|a| *a != song.title), duration]),
    };
    let mut class = String::from("row tappable song");
    if song.playing {
        class += " playing";
    }
    match song.mark {
        Some(LoopMark::A) => class += " loop loop-a",
        Some(LoopMark::Inside) => class += " loop",
        Some(LoopMark::B) => class += " loop loop-b",
        Some(LoopMark::AB) => class += " loop loop-a loop-b",
        None => {}
    }
    let badge = match song.mark {
        Some(LoopMark::A) => Some("A"),
        Some(LoopMark::B) => Some("B"),
        Some(LoopMark::AB) => Some("A B"),
        _ => None,
    };
    rsx! {
        li { class, onclick: move |_| onplay.call(()),
            match index {
                Some(n) => rsx! {
                    span { class: "index",
                        if song.playing { Equalizer {} } else { "{n}" }
                    }
                },
                None => rsx! {
                    div { class: "cover-wrap",
                        Cover { url: song.art.clone() }
                        if song.playing { div { class: "cover-eq", Equalizer {} } }
                    }
                },
            }
            div { class: "meta",
                div { class: "title", "{song.title}" }
                div { class: "sub", "{sub}" }
            }
            if let Some(badge) = badge {
                span { class: "loop-badge", "{badge}" }
            }
            if let Some(onmore) = onmore {
                MoreButton { onmore }
            }
        }
    }
}

/// Bouncing bars marking the playing song.
#[component]
fn Equalizer() -> Element {
    rsx! {
        span { class: "eq", "aria-label": "Playing", span {} span {} span {} }
    }
}

#[component]
pub fn AlbumGrid(albums: Vec<AlbumItem>, onopen: EventHandler<usize>, #[props(default)] shelf: bool) -> Element {
    rsx! {
        ul { class: if shelf { "grid shelf" } else { "grid" },
            for (i , album) in albums.into_iter().enumerate() {
                li { key: "{album.artist}/{album.title}", class: "tile", onclick: move |_| onopen.call(i),
                    Cover { url: album.art.clone(), class: "cover square", icon: Icon::Disc }
                    div { class: "title", "{album.title}" }
                    div { class: "sub", {dotted([Some(album.artist.clone()), album.year.map(|y| y.to_string())])} }
                }
            }
        }
    }
}

#[component]
fn ArtistRow(artist: ArtistItem, onopen: EventHandler<()>) -> Element {
    let sub = dotted([
        Some(plural(artist.tracks as usize, "song", "songs")),
        (artist.albums > 0).then(|| plural(artist.albums as usize, "album", "albums")),
    ]);
    rsx! {
        li { class: "row tappable", onclick: move |_| onopen.call(()),
            Cover { url: artist.art.clone(), class: "cover round", icon: Icon::Person }
            div { class: "meta",
                div { class: "title", "{artist.name}" }
                div { class: "sub", "{sub}" }
            }
            span { class: "chevron", Svg { icon: Icon::ChevronRight, size: 20 } }
        }
    }
}

/// Play and shuffle, plus a menu button when `onmore` is set; disabled when
/// there is nothing to play.
#[component]
fn PlayButtons(
    onplay: EventHandler<()>,
    onshuffle: EventHandler<()>,
    #[props(default)] onmore: Option<EventHandler<()>>,
    #[props(default)] empty: bool,
) -> Element {
    rsx! {
        div { class: "hero-actions",
            button { class: "primary", disabled: empty, onclick: move |_| onplay.call(()), Svg { icon: Icon::Play, size: 20 } "Play" }
            button { class: "secondary", disabled: empty, onclick: move |_| onshuffle.call(()), Svg { icon: Icon::Shuffle, size: 20 } "Shuffle" }
            if let Some(onmore) = onmore {
                button { class: "secondary round", "aria-label": "More", onclick: move |_| onmore.call(()), Svg { icon: Icon::More, size: 20 } }
            }
        }
    }
}

/// A downloaded album. `header.cover` is the large art.
#[component]
pub fn LocalAlbumPage(
    header: AlbumHeader,
    songs: Vec<SongItem>,
    onback: EventHandler<()>,
    onplay: EventHandler<usize>,
    onshuffle: EventHandler<()>,
    /// A song's menu.
    onmore: EventHandler<usize>,
    /// The album's menu.
    onalbummore: EventHandler<()>,
) -> Element {
    let total: f64 = songs.iter().filter_map(|s| s.duration_secs).sum();
    let line = dotted([Some(plural(songs.len(), "song", "songs")), (total > 0.0).then(|| total_text(total))]);
    rsx! {
        div { class: "album",
            BackButton { onback }
            Hero { cover: header.cover.clone(), title: header.title.clone(), meta: header.meta(), icon: Icon::Disc,
                div { class: "sub", "{line}" }
                PlayButtons { onplay: move |_| onplay.call(0), onshuffle, onmore: onalbummore, empty: songs.is_empty() }
            }
            SongList { songs, onplay, onmore, numbered: true }
        }
    }
}

#[component]
pub fn PlaylistPage(
    name: String,
    cover: Option<String>,
    songs: Vec<SongItem>,
    /// Set for a synced playlist.
    #[props(default)]
    sync: Option<SyncView>,
    onback: EventHandler<()>,
    onplay: EventHandler<usize>,
    onshuffle: EventHandler<()>,
    /// A song's menu.
    onmore: EventHandler<usize>,
    /// The playlist's menu.
    onplaylistmore: EventHandler<()>,
) -> Element {
    let total: f64 = songs.iter().filter_map(|s| s.duration_secs).sum();
    let meta = dotted([
        Some(if sync.is_some() { "Synced playlist" } else { "Playlist" }.into()),
        Some(plural(songs.len(), "song", "songs")),
        (total > 0.0).then(|| total_text(total)),
    ]);
    let waiting = matches!(sync, Some(SyncView::Syncing | SyncView::Synced { pending: 1.., .. }));
    rsx! {
        div { class: "album",
            BackButton { onback }
            Hero { cover, title: name, meta, icon: Icon::Playlist,
                match sync {
                    Some(SyncView::Syncing) => rsx! {
                        div { class: "sync-line", div { class: "spinner tiny" } "Syncing with YouTube…" }
                    },
                    Some(SyncView::Failed(e)) => rsx! {
                        div { class: "sync-line failed", Svg { icon: Icon::Alert, size: 16 } "Couldn't sync: {e}" }
                    },
                    Some(SyncView::Synced { ago, pending }) => rsx! {
                        div { class: "sync-line",
                            Svg { icon: Icon::Sync, size: 16 }
                            {dotted([Some(format!("Synced with YouTube {ago}")), (pending > 0).then(|| format!("{pending} to download"))])}
                        }
                    },
                    None => rsx! {},
                }
                PlayButtons { onplay: move |_| onplay.call(0), onshuffle, onmore: onplaylistmore, empty: songs.is_empty() }
            }
            if songs.is_empty() && waiting {
                EmptyState {
                    icon: Icon::Download,
                    title: "Downloading your playlist",
                    text: "Songs show up here as they finish.",
                }
            } else if songs.is_empty() {
                EmptyState {
                    icon: Icon::Playlist,
                    title: "No songs yet",
                    text: "Add songs from your library with their ⋮ menu.",
                }
            } else {
                SongList { songs, onplay, onmore }
            }
        }
    }
}

#[component]
pub fn ArtistPage(
    name: String,
    art: Option<String>,
    songs: Vec<SongItem>,
    albums: Vec<AlbumItem>,
    onback: EventHandler<()>,
    onplay: EventHandler<usize>,
    onshuffle: EventHandler<()>,
    onalbum: EventHandler<usize>,
    /// A song's menu.
    onmore: EventHandler<usize>,
) -> Element {
    let meta = dotted([
        Some(plural(songs.len(), "song", "songs")),
        (!albums.is_empty()).then(|| plural(albums.len(), "album", "albums")),
    ]);
    rsx! {
        div { class: "album",
            BackButton { onback }
            Hero { cover: art, title: name, meta, round: true, icon: Icon::Person,
                PlayButtons { onplay: move |_| onplay.call(0), onshuffle }
            }
            if !albums.is_empty() {
                h2 { class: "section", "Albums" }
                AlbumGrid { albums, onopen: onalbum, shelf: true }
            }
            h2 { class: "section", "Songs" }
            SongList { songs, onplay, onmore }
        }
    }
}

// ---- player ----

/// The bar above the tabs while something is loaded; tapping it opens the player.
#[component]
pub fn MiniPlayer(
    now: NowItem,
    /// 0..1 through the song.
    progress: f64,
    playing: bool,
    ontoggle: EventHandler<()>,
    onnext: EventHandler<()>,
    onopen: EventHandler<()>,
) -> Element {
    let pct = progress.clamp(0.0, 1.0) * 100.0;
    rsx! {
        div { class: "mini", onclick: move |_| onopen.call(()),
            div { class: "mini-progress", div { class: "mini-progress-fill", style: "width: {pct:.2}%" } }
            Cover { url: now.art.clone() }
            div { class: "meta",
                div { class: "title", "{now.title}" }
                div { class: "sub", "{now.artists}" }
            }
            button {
                class: "icon-btn",
                "aria-label": if playing { "Pause" } else { "Play" },
                onclick: move |e| {
                    e.stop_propagation();
                    ontoggle.call(());
                },
                Svg { icon: if playing { Icon::Pause } else { Icon::Play } }
            }
            button {
                class: "icon-btn",
                "aria-label": "Next",
                onclick: move |e| {
                    e.stop_propagation();
                    onnext.call(());
                },
                Svg { icon: Icon::Next }
            }
        }
    }
}

#[component]
pub fn NowPlayingPage(
    now: NowItem,
    position: f64,
    duration: f64,
    playing: bool,
    buffering: bool,
    repeat: RepeatMode,
    /// The song's A-B loop in seconds: A, and B once it is set.
    song_loop: Option<(f64, Option<f64>)>,
    /// The whole queue; the current song is marked playing.
    queue: Vec<SongItem>,
    queue_loop: QueueLoopView,
    onclose: EventHandler<()>,
    ontoggle: EventHandler<()>,
    onnext: EventHandler<()>,
    onprevious: EventHandler<()>,
    /// While the slider is dragged.
    onseeking: EventHandler<f64>,
    /// When it is let go.
    onseek: EventHandler<f64>,
    onrepeat: EventHandler<()>,
    /// The A-B button: sets A, then B, then clears.
    onab: EventHandler<()>,
    /// A queue row tapped (to play it, or to pick it for the loop).
    onskip: EventHandler<usize>,
    /// A queue row's menu.
    onmore: EventHandler<usize>,
    /// The queue loop button: starts picking, or cancels or stops the loop.
    onqueueloop: EventHandler<()>,
) -> Element {
    let max = duration.max(1.0);
    let pct = (position / max * 100.0).clamp(0.0, 100.0);
    let at = |secs: f64| (secs / max * 100.0).clamp(0.0, 100.0);
    let (ab_class, ab_label) = match song_loop {
        None => ("ab-btn", "A-B loop off"),
        Some((_, None)) => ("ab-btn pending", "Set B"),
        Some((_, Some(_))) => ("ab-btn on", "Stop A-B loop"),
    };
    let ab_note = match song_loop {
        None => None,
        Some((a, None)) => Some(format!("A {} · tap A-B again for B", duration_text(a))),
        Some((a, Some(b))) => Some(format!("Looping {} – {}", duration_text(a), duration_text(b))),
    };
    let sub = dotted([Some(now.artists.clone()), now.album.clone().filter(|a| *a != now.title)]);
    let parse = |v: String| v.parse::<f64>().ok();
    rsx! {
        div { class: "now",
            div { class: "now-backdrop",
                if let Some(url) = now.art_large.clone() {
                    Img { url, class: "hero-bg" }
                }
            }
            header { class: "now-bar",
                button { class: "icon-btn", "aria-label": "Close", onclick: move |_| onclose.call(()),
                    Svg { icon: Icon::ChevronDown, size: 28 }
                }
                span { "Now playing" }
                div { class: "icon-btn" }
            }
            Cover { url: now.art_large.clone(), class: "cover now-cover" }
            div { class: "now-meta",
                h1 { "{now.title}" }
                div { class: "sub", "{sub}" }
            }
            div { class: "seek",
                if let Some((a, b)) = song_loop {
                    div { class: "ab-track",
                        if let Some(b) = b {
                            div { class: "ab-range", style: "left: {at(a):.2}%; width: {at(b) - at(a):.2}%" }
                        }
                        div { class: "ab-mark", style: "left: {at(a):.2}%" }
                        if let Some(b) = b {
                            div { class: "ab-mark", style: "left: {at(b):.2}%" }
                        }
                    }
                }
                input {
                    r#type: "range",
                    "aria-label": "Position",
                    min: "0",
                    max: "{max}",
                    step: "0.1",
                    value: "{position}",
                    style: "--p: {pct:.2}%",
                    oninput: move |e| {
                        if let Some(v) = parse(e.value()) {
                            onseeking.call(v);
                        }
                    },
                    onchange: move |e| {
                        if let Some(v) = parse(e.value()) {
                            onseek.call(v);
                        }
                    },
                }
                div { class: "times",
                    span { {duration_text(position)} }
                    if let Some(note) = ab_note {
                        span { class: "ab-note", "{note}" }
                    }
                    span { {duration_text(duration)} }
                }
            }
            div { class: "controls",
                button {
                    class: if repeat == RepeatMode::Off { "icon-btn" } else { "icon-btn on" },
                    "aria-label": match repeat {
                        RepeatMode::Off => "Repeat off",
                        RepeatMode::All => "Repeat all",
                        RepeatMode::One => "Repeat one",
                    },
                    onclick: move |_| onrepeat.call(()),
                    Svg { icon: if repeat == RepeatMode::One { Icon::RepeatOne } else { Icon::Repeat } }
                }
                button { class: "icon-btn big", "aria-label": "Previous", onclick: move |_| onprevious.call(()),
                    Svg { icon: Icon::Previous, size: 32 }
                }
                button { class: "play-btn", "aria-label": if playing { "Pause" } else { "Play" }, onclick: move |_| ontoggle.call(()),
                    if playing && buffering {
                        div { class: "spinner dark" }
                    } else {
                        Svg { icon: if playing { Icon::Pause } else { Icon::Play }, size: 34 }
                    }
                }
                button { class: "icon-btn big", "aria-label": "Next", onclick: move |_| onnext.call(()),
                    Svg { icon: Icon::Next, size: 32 }
                }
                button { class: ab_class, "aria-label": ab_label, onclick: move |_| onab.call(()),
                    span { class: "a", "A" }
                    "-"
                    span { class: "b", "B" }
                }
            }
            h2 { class: "section",
                "Queue"
                span { class: "section-note",
                    match queue_loop {
                        QueueLoopView::Off => plural(queue.len(), "song", "songs"),
                        QueueLoopView::PickA => "Tap the song to loop back to (A)".into(),
                        QueueLoopView::PickB => "Tap the last song of the loop (B)".into(),
                        QueueLoopView::Looping { first, last } if first == last => format!("Looping song {first}"),
                        QueueLoopView::Looping { first, last } => format!("Looping songs {first}–{last}"),
                    }
                }
                button { class: "text-btn section-action", onclick: move |_| onqueueloop.call(()),
                    match queue_loop {
                        QueueLoopView::Off => "A-B loop",
                        QueueLoopView::PickA | QueueLoopView::PickB => "Cancel",
                        QueueLoopView::Looping { .. } => "Stop loop",
                    }
                }
            }
            div { class: if matches!(queue_loop, QueueLoopView::PickA | QueueLoopView::PickB) { "queue picking" } else { "queue" },
                SongList { songs: queue, onplay: onskip, onmore }
            }
        }
    }
}

// ---- menus and dialogs ----

/// A bottom sheet of actions; tapping outside closes it.
#[component]
pub fn MenuSheet(head: Option<MenuHead>, items: Vec<MenuItem>, onpick: EventHandler<usize>, onclose: EventHandler<()>) -> Element {
    rsx! {
        div { class: "modal",
            div { class: "scrim", onclick: move |_| onclose.call(()) }
            div { class: "menu", role: "menu",
                div { class: "grip" }
                if let Some(head) = head {
                    div { class: "menu-head",
                        Cover { url: head.art.clone(), icon: head.icon }
                        div { class: "meta",
                            div { class: "title", "{head.title}" }
                            div { class: "sub", "{head.sub}" }
                        }
                    }
                }
                for (i , item) in items.into_iter().enumerate() {
                    button {
                        key: "{i}",
                        class: if item.danger { "menu-item danger" } else { "menu-item" },
                        role: "menuitem",
                        onclick: move |_| onpick.call(i),
                        Svg { icon: item.icon }
                        div { class: "meta",
                            div { class: "title", "{item.label}" }
                            if let Some(sub) = item.sub {
                                div { class: "sub", "{sub}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A question with a confirm button, and a text field when `value` is set.
#[component]
pub fn Dialog(
    title: String,
    #[props(default)] text: Option<String>,
    /// The text field's contents; no field when `None`.
    #[props(default)] value: Option<String>,
    #[props(default)] placeholder: String,
    confirm: String,
    #[props(default)] danger: bool,
    #[props(default)] oninput: Option<EventHandler<String>>,
    onconfirm: EventHandler<()>,
    oncancel: EventHandler<()>,
) -> Element {
    let blank = value.as_deref().is_some_and(|v| v.trim().is_empty());
    rsx! {
        div { class: "modal",
            div { class: "scrim", onclick: move |_| oncancel.call(()) }
            form {
                class: "dialog",
                role: "dialog",
                onsubmit: move |e| {
                    e.prevent_default();
                    if !blank {
                        onconfirm.call(());
                    }
                },
                h2 { "{title}" }
                if let Some(text) = text {
                    p { "{text}" }
                }
                if let Some(value) = value {
                    input {
                        r#type: "text",
                        "enterkeyhint": "done",
                        placeholder: "{placeholder}",
                        value: "{value}",
                        maxlength: "100",
                        oninput: move |e| {
                            if let Some(h) = oninput {
                                h.call(e.value());
                            }
                        },
                        onmounted: move |e| async move {
                            let _ = e.set_focus(true).await;
                        },
                    }
                }
                div { class: "dialog-actions",
                    button { r#type: "button", class: "text-btn", onclick: move |_| oncancel.call(()), "Cancel" }
                    button { r#type: "submit", class: if danger { "text-btn strong danger" } else { "text-btn strong" }, disabled: blank,
                        "{confirm}"
                    }
                }
            }
        }
    }
}

// ---- downloads ----

#[component]
pub fn DownloadsPage(
    jobs: Vec<Job>,
    storage: bool,
    oncancel: EventHandler<u64>,
    oncancelall: EventHandler<()>,
    onretry: EventHandler<u64>,
    onclear: EventHandler<()>,
    onallow: EventHandler<()>,
) -> Element {
    let (active, finished): (Vec<Job>, Vec<Job>) = jobs.into_iter().rev().partition(Job::is_active);
    let waiting = active.iter().filter(|j| j.state == JobState::Queued).count();
    let running = active.len() - waiting;
    let summary = dotted([
        (running > 0).then(|| format!("{running} downloading")),
        (waiting > 0).then(|| format!("{waiting} waiting")),
    ]);
    rsx! {
        header { class: "topbar plain",
            h1 { "Downloads" }
            if !finished.is_empty() {
                button { class: "text-btn", onclick: move |_| onclear.call(()), "Clear finished" }
            }
        }
        if !storage {
            StorageBanner { onallow }
        }
        if active.is_empty() && finished.is_empty() {
            EmptyState {
                icon: Icon::Download,
                title: "No downloads yet",
                text: "Songs you download show up here, and in your music player.",
            }
        }
        if !active.is_empty() {
            h2 { class: "section",
                "In progress"
                span { class: "section-note", "{summary}" }
                button { class: "text-btn section-action", onclick: move |_| oncancelall.call(()), "Cancel all" }
            }
            ul { class: "list",
                for job in active {
                    JobRow { key: "{job.id}", job, oncancel, onretry }
                }
            }
        }
        if !finished.is_empty() {
            h2 { class: "section", "Finished" }
            ul { class: "list",
                for job in finished {
                    JobRow { key: "{job.id}", job, oncancel, onretry }
                }
            }
        }
    }
}

#[component]
fn JobRow(job: Job, oncancel: EventHandler<u64>, onretry: EventHandler<u64>) -> Element {
    let id = job.id;
    let artists = job.entry.artists.join(", ");
    let fraction = job.progress.as_ref().and_then(|p| p.fraction());
    let (status, status_class) = match &job.state {
        JobState::Queued => ("Waiting…".to_string(), ""),
        JobState::Downloading => {
            let p = job.progress.as_ref();
            let size = match (p.and_then(|p| p.downloaded_bytes), p.and_then(|p| p.total_bytes)) {
                (Some(d), Some(t)) => Some(format!("{} of {} MB", mb(d), mb(t))),
                (Some(d), None) => Some(format!("{} MB", mb(d))),
                _ => None,
            };
            let speed = p.and_then(|p| p.speed).map(|s| format!("{} MB/s", mb(s as u64)));
            let eta = p.and_then(|p| p.eta).map(|s| format!("{} left", duration_text(s)));
            let text = dotted([size, speed, eta]);
            (if text.is_empty() { "Starting…".to_string() } else { text }, "")
        }
        JobState::Done { path, tagged } => {
            let place = if path.to_string_lossy().contains("/Android/data/") { "Saved in the app's folder" } else { "Saved to Music" };
            (if *tagged { place.to_string() } else { format!("{place} • without tags") }, "")
        }
        JobState::Failed(e) => (e.clone(), "failed"),
        JobState::Cancelled => ("Cancelled".to_string(), ""),
    };
    rsx! {
        li { class: "row job",
            Cover { url: job.entry.thumbnail.clone() }
            div { class: "meta",
                div { class: "title", "{job.entry.title}" }
                div { class: "sub", "{artists}" }
                match job.state {
                    JobState::Queued => rsx! { div { class: "bar indeterminate", div { class: "bar-fill" } } },
                    JobState::Downloading => {
                        let pct = fraction.unwrap_or(0.0) * 100.0;
                        rsx! { div { class: "bar", div { class: "bar-fill", style: "width: {pct:.1}%" } } }
                    }
                    _ => rsx! {},
                }
                div { class: "status {status_class}", "{status}" }
            }
            match job.state {
                JobState::Queued | JobState::Downloading => rsx! {
                    button { class: "icon-btn", "aria-label": "Cancel", onclick: move |_| oncancel.call(id),
                        Svg { icon: Icon::Close }
                    }
                },
                JobState::Done { .. } => rsx! {
                    div { class: "icon-btn", span { class: "done-badge", Svg { icon: Icon::Check, size: 16 } } }
                },
                JobState::Failed(_) | JobState::Cancelled => rsx! {
                    button { class: "icon-btn failed", "aria-label": "Retry", onclick: move |_| onretry.call(id),
                        Svg { icon: Icon::Retry }
                    }
                },
            }
        }
    }
}

// ---- settings ----

#[component]
pub fn SettingsPage(
    about: About,
    output: String,
    storage: bool,
    update: Option<String>,
    checking: bool,
    onupdate: EventHandler<Channel>,
    onallow: EventHandler<()>,
) -> Element {
    rsx! {
        header { class: "topbar plain", h1 { "Settings" } }
        section { class: "group",
            h2 { "Storage" }
            div { class: "card",
                div { class: "item",
                    Svg { icon: Icon::Folder }
                    div { class: "meta",
                        div { class: "title", "Save location" }
                        div { class: "sub", "{output}" }
                    }
                }
                if !storage {
                    div { class: "item",
                        Svg { icon: Icon::Alert }
                        div { class: "meta",
                            div { class: "title", "All-files access is off" }
                            div { class: "sub", "Downloads stay in the app's folder, where music apps can't see them." }
                        }
                        button { class: "primary small", onclick: move |_| onallow.call(()), "Allow" }
                    }
                }
            }
        }
        section { class: "group",
            h2 { "yt-dlp" }
            div { class: "card",
                div { class: "item",
                    Svg { icon: Icon::Download }
                    div { class: "meta",
                        div { class: "title", "Version" }
                        div { class: "sub", "{about.yt_dlp}" }
                    }
                }
                div { class: "actions",
                    button { class: "secondary", disabled: checking, onclick: move |_| onupdate.call(Channel::Stable), "Check for updates" }
                    button { class: "secondary", disabled: checking, onclick: move |_| onupdate.call(Channel::Nightly), "Try nightly" }
                }
                if let Some(msg) = update {
                    div { class: "note", "{msg}" }
                }
            }
            p { class: "hint", "YouTube changes often. If downloads start failing, update yt-dlp, then restart the app." }
        }
        section { class: "group",
            h2 { "About" }
            div { class: "card",
                AboutItem { label: "ytmdl", value: about.app }
                AboutItem { label: "Python", value: about.python }
                AboutItem { label: "TLS", value: about.openssl }
            }
        }
    }
}

#[component]
fn AboutItem(label: String, value: String) -> Element {
    rsx! {
        div { class: "item",
            div { class: "meta",
                div { class: "title", "{label}" }
                div { class: "sub", "{value}" }
            }
        }
    }
}
