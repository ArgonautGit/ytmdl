use dioxus::prelude::*;
use ytmdl_core::{Channel, Entry, Resolved, Runtime, SearchSource};

use crate::jobs::{Job, JobState, Queue, Services};
use crate::platform;

const CSS: &str = include_str!("../assets/style.css");

#[derive(Clone)]
pub enum Boot {
    Starting,
    Ready(Services),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Search,
    Downloads,
    Settings,
}

#[derive(Clone, Copy)]
struct Ctx {
    boot: Signal<Boot>,
    queue: Queue,
    /// All-files access, i.e. whether downloads go to the shared Music folder.
    storage: Signal<bool>,
}

impl Ctx {
    fn services(&self) -> Option<Services> {
        match &*self.boot.read() {
            Boot::Ready(s) => Some(s.clone()),
            _ => None,
        }
    }
}

#[component]
pub fn App() -> Element {
    let boot = use_signal(|| Boot::Starting);
    let queue = use_hook(Queue::new);
    let storage = use_signal(platform::has_storage_access);
    let ctx = use_context_provider(|| Ctx { boot, queue, storage });
    let mut tab = use_signal(|| Tab::Search);
    use_hook(move || spawn(crate::start(ctx.boot, ctx.queue)));
    use_hook(move || watch_storage_access(storage));

    let active = queue.jobs().read().iter().filter(|j| j.is_active()).count();
    let downloads_label = if active > 0 { format!("Downloads ({active})") } else { "Downloads".to_string() };
    let body = match &*boot.read() {
        Boot::Starting => rsx! {
            div { class: "status", "Starting Python and yt-dlp…" }
        },
        Boot::Failed(e) => rsx! {
            div { class: "status error",
                h3 { "Could not start" }
                pre { "{e}" }
            }
        },
        // Every tab stays mounted, so search results survive a look at the queue.
        Boot::Ready(_) => rsx! {
            div { hidden: tab() != Tab::Search, SearchView {} }
            div { hidden: tab() != Tab::Downloads, DownloadsView {} }
            div { hidden: tab() != Tab::Settings, SettingsView {} }
        },
    };

    rsx! {
        style { {CSS} }
        div { class: "app",
            nav { class: "tabs",
                TabButton { label: "Search", selected: tab() == Tab::Search, onclick: move |_| tab.set(Tab::Search) }
                TabButton { label: downloads_label, selected: tab() == Tab::Downloads, onclick: move |_| tab.set(Tab::Downloads) }
                TabButton { label: "Settings", selected: tab() == Tab::Settings, onclick: move |_| tab.set(Tab::Settings) }
            }
            main {
                if !storage() {
                    StorageBanner {}
                }
                {body}
            }
        }
    }
}

#[component]
fn StorageBanner() -> Element {
    rsx! {
        div { class: "banner",
            span { "Allow access to all files so downloads go to your Music folder. Until then they stay in the app's own folder, where music apps can't see them." }
            button { onclick: move |_| platform::request_storage_access(), "Allow" }
        }
    }
}

/// Access is granted on a system settings page with no callback to the app, so
/// poll until it shows up.
fn watch_storage_access(mut storage: Signal<bool>) {
    if *storage.peek() {
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let polling = std::thread::Builder::new().name("ytmdl-storage".into()).spawn(move || {
        while !tx.is_closed() {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if platform::has_storage_access() {
                let _ = tx.send(());
                return;
            }
        }
    });
    if polling.is_ok() {
        spawn(async move {
            if rx.recv().await.is_some() {
                storage.set(true);
            }
        });
    }
}

#[component]
fn TabButton(#[props(into)] label: String, selected: bool, onclick: EventHandler<MouseEvent>) -> Element {
    rsx! {
        button { class: if selected { "tab selected" } else { "tab" }, onclick: move |e| onclick.call(e), "{label}" }
    }
}

#[derive(Clone, PartialEq)]
enum Results {
    Idle,
    Loading,
    Entries(Vec<Entry>),
    Album { title: String, entries: Vec<Entry> },
    Error(String),
}

#[component]
fn SearchView() -> Element {
    let ctx = use_context::<Ctx>();
    let mut query = use_signal(String::new);
    let mut source = use_signal(|| SearchSource::MusicSongs);
    let mut results = use_signal(|| Results::Idle);

    let mut search = move || {
        let q = query.read().trim().to_string();
        let Some(svc) = ctx.services() else { return };
        if q.is_empty() {
            return;
        }
        results.set(Results::Loading);
        spawn(async move {
            // A pasted link is resolved instead of searched.
            let outcome = if q.starts_with("http://") || q.starts_with("https://") {
                svc.dl.resolve(&q).await.map(|r| match r {
                    Resolved::Track(t) => Results::Entries(vec![Entry {
                        id: t.id,
                        url: t.url,
                        title: t.title,
                        artists: t.artists,
                        album: t.album,
                        duration_secs: t.duration_secs,
                        thumbnail: t.cover_urls.into_iter().next(),
                    }]),
                    Resolved::Collection { title, entries, .. } => Results::Album { title, entries },
                })
            } else {
                svc.dl.search(&q, 20, source()).await.map(Results::Entries)
            };
            results.set(outcome.unwrap_or_else(|e| Results::Error(e.to_string())));
        });
    };

    let mut open_album = move |url: String| {
        let Some(svc) = ctx.services() else { return };
        results.set(Results::Loading);
        spawn(async move {
            results.set(match svc.dl.resolve(&url).await {
                Ok(Resolved::Collection { title, entries, .. }) => Results::Album { title, entries },
                Ok(Resolved::Track(t)) => Results::Error(format!("{} is a single track", t.title)),
                Err(e) => Results::Error(e.to_string()),
            });
        });
    };

    let albums = source() == SearchSource::MusicAlbums;
    rsx! {
        form {
            class: "search",
            onsubmit: move |e| {
                e.prevent_default();
                search();
            },
            input {
                r#type: "search",
                placeholder: "Search or paste a link",
                value: "{query}",
                oninput: move |e| query.set(e.value()),
            }
            button { r#type: "submit", "Go" }
        }
        div { class: "sources",
            for (label , value) in [("Songs", SearchSource::MusicSongs), ("Albums", SearchSource::MusicAlbums), ("YouTube", SearchSource::YouTube)] {
                button {
                    class: if source() == value { "chip selected" } else { "chip" },
                    onclick: move |_| source.set(value),
                    "{label}"
                }
            }
        }
        match results() {
            Results::Idle => rsx! {},
            Results::Loading => rsx! { div { class: "status", "Loading…" } },
            Results::Error(e) => rsx! { div { class: "status error", "{e}" } },
            Results::Entries(entries) => rsx! {
                if entries.is_empty() {
                    div { class: "status", "No results" }
                }
                ul { class: "list",
                    for entry in entries {
                        EntryRow { key: "{entry.id}", entry: entry.clone(), album: albums, onopen: move |url| open_album(url) }
                    }
                }
            },
            Results::Album { title, entries } => {
                let count = entries.len();
                let all = entries.clone();
                rsx! {
                div { class: "album-head",
                    h3 { "{title}" }
                    button {
                        onclick: move |_| {
                            let Some(svc) = ctx.services() else { return };
                            for e in &all {
                                ctx.queue.start(svc.clone(), e.url.clone(), e.title.clone(), e.artists.join(", "));
                            }
                        },
                        "Download all ({count})"
                    }
                }
                ul { class: "list",
                    for entry in entries {
                        EntryRow { key: "{entry.id}", entry: entry.clone(), album: false, onopen: move |url| open_album(url) }
                    }
                }
                }
            }
        }
    }
}

#[component]
fn EntryRow(entry: Entry, album: bool, onopen: EventHandler<String>) -> Element {
    let ctx = use_context::<Ctx>();
    let artists = entry.artists.join(", ");
    let duration = entry.duration_secs.map(|d| format!("{}:{:02}", d as u64 / 60, d as u64 % 60)).unwrap_or_default();
    let title = entry.title.clone();
    let url = entry.url.clone();
    rsx! {
        li { class: "row",
            div { class: "meta",
                div { class: "title", "{title}" }
                div { class: "sub", "{artists} {duration}" }
            }
            if album {
                button { onclick: move |_| onopen.call(url.clone()), "Open" }
            } else {
                button {
                    onclick: move |_| {
                        if let Some(svc) = ctx.services() {
                            ctx.queue.start(svc, entry.url.clone(), entry.title.clone(), entry.artists.join(", "));
                        }
                    },
                    "Download"
                }
            }
        }
    }
}

#[component]
fn DownloadsView() -> Element {
    let ctx = use_context::<Ctx>();
    let jobs: Vec<Job> = ctx.queue.jobs().read().iter().rev().cloned().collect();
    rsx! {
        div { class: "toolbar",
            button { onclick: move |_| ctx.queue.clear_finished(), "Clear finished" }
        }
        if jobs.is_empty() {
            div { class: "status", "Nothing downloaded yet" }
        }
        ul { class: "list",
            {jobs.into_iter().map(job_row)}
        }
    }
}

fn job_row(job: Job) -> Element {
    let status = match &job.state {
        JobState::Queued => "Queued".to_string(),
        JobState::Downloading => progress_text(job.progress.as_ref()),
        JobState::Done { path, tagged } => format!("{}{}", path.display(), if *tagged { "" } else { " (untagged)" }),
        JobState::Failed(e) => format!("Failed: {e}"),
        JobState::Cancelled => "Cancelled".to_string(),
    };
    let fraction = job.progress.as_ref().and_then(|p| p.fraction()).unwrap_or(0.0);
    let downloading = job.state == JobState::Downloading;
    let active = job.is_active();
    let cancel = job.cancel.clone();
    rsx! {
        li { key: "{job.id}", class: "row",
            div { class: "meta",
                div { class: "title", "{job.title}" }
                div { class: "sub", "{job.artist}" }
                div { class: "sub", "{status}" }
                if downloading {
                    progress { max: "1", value: "{fraction}" }
                }
            }
            if active {
                button { onclick: move |_| cancel.cancel(), "Cancel" }
            }
        }
    }
}

fn progress_text(p: Option<&ytmdl_core::Progress>) -> String {
    let Some(p) = p else { return "Starting…".into() };
    let mb = |b: u64| b as f64 / 1e6;
    match (p.downloaded_bytes, p.total_bytes) {
        (Some(d), Some(t)) => format!("{:.1} / {:.1} MB", mb(d), mb(t)),
        (Some(d), None) => format!("{:.1} MB", mb(d)),
        _ => p.status.clone(),
    }
}

#[component]
fn SettingsView() -> Element {
    let ctx = use_context::<Ctx>();
    let mut update = use_signal(|| None::<String>);
    let Some(svc) = ctx.services() else { return rsx! {} };
    let rt: Runtime = svc.dl.runtime().clone();
    let v = rt.version().clone();
    let yt_dlp = v.yt_dlp.clone().unwrap_or_else(|| "?".into());
    let output = if (ctx.storage)() { &svc.output_dir } else { &svc.fallback_output_dir };
    let output = output.display().to_string();

    let check = move |channel: Channel| {
        let rt = rt.clone();
        update.set(Some("Checking…".into()));
        spawn(async move {
            update.set(Some(match rt.check_update(channel).await {
                Ok(o) if o.updated => format!("Downloaded yt-dlp {}; restart the app to use it", o.version),
                Ok(o) => format!("yt-dlp {} is current", o.version),
                Err(e) => format!("Update failed: {e}"),
            }));
        });
    };
    let mut check_nightly = check.clone();
    let mut check_stable = check;

    rsx! {
        dl { class: "facts",
            dt { "yt-dlp" }
            dd { "{yt_dlp}" }
            dt { "Python" }
            dd { "{v.python} ({v.platform})" }
            dt { "TLS" }
            dd { "{v.openssl}" }
            dt { "Saving to" }
            dd { "{output}" }
        }
        div { class: "toolbar",
            button { onclick: move |_| check_stable(Channel::Stable), "Update yt-dlp" }
            button { onclick: move |_| check_nightly(Channel::Nightly), "Try nightly" }
        }
        if let Some(msg) = update() {
            div { class: "status", "{msg}" }
        }
    }
}
