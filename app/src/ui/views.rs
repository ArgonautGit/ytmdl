//! Presentational pieces: plain props in, events out. They hold no app state, so
//! `preview` can render every screen with sample data.

use dioxus::prelude::*;
use ytmdl_core::{Channel, Entry, SearchSource, art_url};

use super::icons::{Icon, Svg};
use crate::jobs::{Job, JobState};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Search,
    Downloads,
    Settings,
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

fn plural(n: usize, one: &str, many: &str) -> String {
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
pub fn Cover(url: Option<String>, #[props(default = "cover".to_string())] class: String) -> Element {
    rsx! {
        div { class: "{class}",
            Svg { icon: Icon::Music, size: 20 }
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

#[component]
pub fn AlbumPage(
    header: AlbumHeader,
    tracks: AlbumTracks,
    onback: EventHandler<()>,
    ondownload: EventHandler<Entry>,
    ondownloadall: EventHandler<()>,
    onretry: EventHandler<()>,
) -> Element {
    let hero = header.cover.as_deref().map(|u| art_url(u, 544));
    let kind = kind_label(header.kind.as_deref()).or(Some("Album".into()));
    let meta = dotted([kind, Some(header.artists.join(", ")), header.year.map(|y| y.to_string())]);
    rsx! {
        div { class: "album",
            button { class: "icon-btn back", "aria-label": "Back", onclick: move |_| onback.call(()),
                Svg { icon: Icon::Back }
            }
            div { class: "album-head",
                div { class: "hero-wrap",
                    if let Some(url) = hero.clone() {
                        Img { url, class: "hero-bg" }
                    }
                    div { class: "hero-fade" }
                }
                div { class: "hero",
                    Cover { url: hero, class: "cover hero-cover" }
                    h1 { "{header.title}" }
                    div { class: "sub", "{meta}" }
                    match &tracks {
                        AlbumTracks::Loaded(rows) => rsx! { AlbumSummary { rows: rows.clone(), ondownloadall } },
                        _ => rsx! {},
                    }
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
fn AlbumSummary(rows: Vec<(Entry, TrackState)>, ondownloadall: EventHandler<()>) -> Element {
    let total: f64 = rows.iter().filter_map(|(e, _)| e.duration_secs).sum();
    let done = rows.iter().filter(|(_, s)| *s == TrackState::Done).count();
    let todo = rows.iter().filter(|(_, s)| s.wants_download()).count();
    let busy = rows.len() - done - todo;
    let line = dotted([Some(plural(rows.len(), "song", "songs")), (total > 0.0).then(|| total_text(total))]);
    rsx! {
        div { class: "sub", "{line}" }
        if todo > 0 {
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
