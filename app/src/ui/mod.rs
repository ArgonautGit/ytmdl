//! App shell and state. The screens are presentational components in `views`,
//! which `preview` also renders to static HTML with sample data.

mod icons;
#[cfg(test)]
mod preview;
mod views;

use std::collections::HashMap;

use dioxus::prelude::*;
use ytmdl_core::{Channel, CollectionKind, Entry, Resolved, SearchSource};

use crate::jobs::{Job, JobState, Queue, Services, entry_from_track};
use crate::platform;
use views::*;

const CSS: &str = include_str!("../../assets/style.css");

#[derive(Clone)]
pub enum Boot {
    Starting,
    Ready(Services),
    Failed(String),
}

#[derive(Clone, Copy)]
struct Ctx {
    boot: Signal<Boot>,
    queue: Queue,
    /// All-files access, i.e. whether downloads go to the shared Music folder.
    storage: Signal<bool>,
    toast: Signal<Option<(u64, String)>>,
}

impl Ctx {
    fn services(&self) -> Option<Services> {
        match &*self.boot.read() {
            Boot::Ready(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn notify(&self, message: String) {
        let mut toast = self.toast;
        let id = toast.peek().as_ref().map_or(0, |(id, _)| id + 1);
        toast.set(Some((id, message)));
    }

    /// Starts `entry`, or retries its failed job; no-op while one is running or done.
    fn download(&self, entry: Entry) {
        let Some(svc) = self.services() else { return };
        let previous = self.queue.jobs().peek().iter().rev().find(|j| j.entry.id == entry.id).map(|j| (j.id, j.state.clone()));
        match previous {
            None | Some((_, JobState::Cancelled)) => self.queue.start(svc, entry),
            Some((id, JobState::Failed(_))) => self.queue.retry(svc, id),
            Some(_) => {}
        }
    }
}

/// Latest download state per video id.
fn track_states(jobs: &[Job]) -> HashMap<String, TrackState> {
    jobs.iter()
        .map(|job| {
            let state = match &job.state {
                JobState::Queued => TrackState::Queued,
                JobState::Downloading => {
                    TrackState::Downloading(job.progress.as_ref().and_then(|p| p.fraction()).unwrap_or(0.0))
                }
                JobState::Done { .. } => TrackState::Done,
                JobState::Failed(_) => TrackState::Failed,
                JobState::Cancelled => TrackState::Idle,
            };
            (job.entry.id.clone(), state)
        })
        .collect()
}

fn with_states(entries: Vec<Entry>, states: &HashMap<String, TrackState>) -> Vec<(Entry, TrackState)> {
    entries
        .into_iter()
        .map(|e| {
            let s = states.get(&e.id).copied().unwrap_or_default();
            (e, s)
        })
        .collect()
}

#[component]
pub fn App() -> Element {
    let boot = use_signal(|| Boot::Starting);
    let queue = use_hook(Queue::new);
    let storage = use_signal(platform::has_storage_access);
    let toast = use_signal(|| None);
    let ctx = use_context_provider(|| Ctx { boot, queue, storage, toast });
    let mut tab = use_signal(|| Tab::Search);
    use_hook(move || spawn(crate::start(ctx.boot, ctx.queue)));
    use_hook(move || watch_storage_access(storage));

    let active = queue.jobs().read().iter().filter(|j| j.is_active()).count();
    let body = match &*boot.read() {
        Boot::Starting => rsx! { StartupScreen {} },
        Boot::Failed(e) => rsx! { StartupScreen { error: e.clone() } },
        // Every tab stays mounted (and keeps its scroll position); only one shows.
        Boot::Ready(_) => rsx! {
            Page { visible: tab() == Tab::Search, SearchScreen {} }
            Page { visible: tab() == Tab::Downloads, DownloadsScreen {} }
            Page { visible: tab() == Tab::Settings, SettingsScreen {} }
            BottomNav { tab: tab(), active, onselect: move |t| tab.set(t) }
        },
    };
    rsx! {
        style { {CSS} }
        div { class: "app",
            {body}
            for (id , message) in toast() {
                Toast { key: "{id}", message }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
enum Found {
    Idle,
    Loading,
    Error(String),
    Entries(SearchSource, Vec<Entry>),
}

#[derive(Clone, PartialEq)]
struct OpenAlbum {
    url: String,
    header: AlbumHeader,
    tracks: Option<Result<Vec<Entry>, String>>,
}

#[component]
fn SearchScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let mut query = use_signal(String::new);
    let mut source = use_signal(|| SearchSource::MusicSongs);
    let mut found = use_signal(|| Found::Idle);
    let mut album = use_signal(|| None::<OpenAlbum>);
    // Bumped per search so a slow earlier search can't overwrite a newer one.
    let mut generation = use_signal(|| 0u64);

    // The album page is a history entry: Android's back gesture (and the page's own
    // back arrow, through history.back()) pops it.
    use_hook(move || {
        spawn(async move {
            let mut popped = document::eval(
                "window.addEventListener('popstate', () => dioxus.send(true)); await new Promise(() => {});",
            );
            while popped.recv::<bool>().await.is_ok() {
                album.set(None);
            }
        })
    });

    let load_album = use_callback(move |url: String| {
        let Some(svc) = ctx.services() else { return };
        spawn(async move {
            let result = svc.dl.resolve(&url).await;
            let mut open = album.write();
            let Some(open) = open.as_mut().filter(|o| o.url == url) else { return };
            open.tracks = Some(match result {
                Ok(Resolved::Collection { entries, .. }) => Ok(entries),
                Ok(Resolved::Track(t)) => Ok(vec![entry_from_track(t)]),
                Err(e) => Err(e.to_string()),
            });
            if open.header.cover.is_none()
                && let Some(Ok(entries)) = &open.tracks
            {
                open.header.cover = entries.iter().find_map(|e| e.thumbnail.clone());
            }
        });
    });

    let show_album = use_callback(move |open: OpenAlbum| {
        let load = open.tracks.is_none().then(|| open.url.clone());
        album.set(Some(open));
        document::eval("history.pushState({ ytmdl: 'album' }, '')");
        if let Some(url) = load {
            load_album.call(url);
        }
    });

    let search = use_callback(move |()| {
        let q = query.peek().trim().to_string();
        let Some(svc) = ctx.services() else { return };
        if q.is_empty() {
            return;
        }
        let src = *source.peek();
        let this = {
            let mut g = generation.write();
            *g += 1;
            *g
        };
        found.set(Found::Loading);
        spawn(async move {
            // A pasted link is resolved instead of searched.
            if q.starts_with("http://") || q.starts_with("https://") {
                let result = svc.dl.resolve(&q).await;
                if *generation.peek() != this {
                    return;
                }
                match result {
                    Ok(Resolved::Track(t)) => found.set(Found::Entries(SearchSource::MusicSongs, vec![entry_from_track(t)])),
                    Ok(Resolved::Collection { title, kind, entries }) => {
                        found.set(Found::Idle);
                        let first = entries.first();
                        show_album.call(OpenAlbum {
                            url: q,
                            header: AlbumHeader {
                                title,
                                artists: first.map(|e| e.artists.clone()).unwrap_or_default(),
                                year: None,
                                kind: Some(if kind == CollectionKind::Album { "album" } else { "playlist" }.into()),
                                cover: entries.iter().find_map(|e| e.thumbnail.clone()),
                            },
                            tracks: Some(Ok(entries)),
                        });
                    }
                    Err(e) => found.set(Found::Error(e.to_string())),
                }
                return;
            }
            let result = svc.dl.search(&q, 20, src).await;
            if *generation.peek() == this {
                found.set(match result {
                    Ok(entries) => Found::Entries(src, entries),
                    Err(e) => Found::Error(e.to_string()),
                });
            }
        });
    });

    let states = track_states(&ctx.queue.jobs().read());
    let results = match found() {
        Found::Idle => ResultsView::Idle,
        Found::Loading => ResultsView::Loading,
        Found::Error(e) => ResultsView::Error(e),
        Found::Entries(SearchSource::MusicAlbums, entries) => ResultsView::Albums(entries),
        Found::Entries(SearchSource::YouTube, entries) => ResultsView::Videos(with_states(entries, &states)),
        Found::Entries(SearchSource::MusicSongs, entries) => ResultsView::Songs(with_states(entries, &states)),
    };
    let open = album();
    rsx! {
        SearchPage {
            query: query(),
            source: source(),
            results,
            storage: (ctx.storage)(),
            oninput: move |q| query.set(q),
            onsubmit: move |_| {
                // Close the keyboard so the results are visible.
                document::eval("document.activeElement && document.activeElement.blur()");
                search.call(());
            },
            onclear: move |_| {
                query.set(String::new());
                found.set(Found::Idle);
                *generation.write() += 1;
            },
            onsource: move |s| {
                source.set(s);
                search.call(());
            },
            ondownload: move |e| ctx.download(e),
            onopen: move |e: Entry| {
                show_album.call(OpenAlbum { url: e.url.clone(), header: AlbumHeader::from_entry(&e), tracks: None })
            },
            onallow: move |_| platform::request_storage_access(),
        }
        if let Some(open) = open {
            div { class: "overlay",
                AlbumPage {
                    tracks: match &open.tracks {
                        None => AlbumTracks::Loading,
                        Some(Err(e)) => AlbumTracks::Error(e.clone()),
                        Some(Ok(entries)) => AlbumTracks::Loaded(with_states(entries.clone(), &states)),
                    },
                    header: open.header.clone(),
                    onback: move |_| {
                        // Close now rather than only on popstate, so a missing history
                        // entry can't leave the page stuck open.
                        album.set(None);
                        document::eval("if (history.state?.ytmdl === 'album') history.back()");
                    },
                    ondownload: move |e| ctx.download(album_track(e, album)),
                    ondownloadall: move |_| {
                        let Some(Some(Ok(entries))) = album.peek().as_ref().map(|o| o.tracks.clone()) else { return };
                        let states = track_states(&ctx.queue.jobs().peek());
                        let todo: Vec<Entry> = entries
                            .into_iter()
                            .filter(|e| states.get(&e.id).copied().unwrap_or_default().wants_download())
                            .collect();
                        let n = todo.len();
                        for e in todo {
                            ctx.download(album_track(e, album));
                        }
                        if n > 0 {
                            ctx.notify(format!("Downloading {n} song{}", if n == 1 { "" } else { "s" }));
                        }
                    },
                    onretry: move |_| {
                        let url = album.peek().as_ref().map(|o| o.url.clone());
                        if let Some(url) = url {
                            if let Some(o) = album.write().as_mut() {
                                o.tracks = None;
                            }
                            load_album.call(url);
                        }
                    },
                }
            }
        }
    }
}

/// Album tracks list video thumbnails; the queue shows the album's square art instead.
fn album_track(mut entry: Entry, album: Signal<Option<OpenAlbum>>) -> Entry {
    if let Some(cover) = album.peek().as_ref().and_then(|o| o.header.cover.clone()) {
        entry.thumbnail = Some(cover);
    }
    entry
}

#[component]
fn DownloadsScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let jobs = ctx.queue.jobs().read().clone();
    rsx! {
        DownloadsPage {
            jobs,
            storage: (ctx.storage)(),
            oncancel: move |id| ctx.queue.cancel(id),
            oncancelall: move |_| {
                let n = ctx.queue.cancel_all();
                if n > 0 {
                    ctx.notify(format!("Cancelled {n} download{}", if n == 1 { "" } else { "s" }));
                }
            },
            onretry: move |id| {
                if let Some(svc) = ctx.services() {
                    ctx.queue.retry(svc, id);
                }
            },
            onclear: move |_| ctx.queue.clear_finished(),
            onallow: move |_| platform::request_storage_access(),
        }
    }
}

#[component]
fn SettingsScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let mut update = use_signal(|| None::<String>);
    let mut checking = use_signal(|| false);
    let Some(svc) = ctx.services() else { return rsx! {} };
    let rt = svc.dl.runtime().clone();
    let v = rt.version().clone();
    let about = About {
        app: env!("CARGO_PKG_VERSION").to_string(),
        yt_dlp: v.yt_dlp.clone().unwrap_or_else(|| "unknown".into()),
        python: format!("{} ({})", v.python, v.platform),
        openssl: v.openssl.clone(),
    };
    let storage = (ctx.storage)();
    let output = if storage { &svc.output_dir } else { &svc.fallback_output_dir };
    rsx! {
        SettingsPage {
            about,
            output: output.display().to_string().replacen("/storage/emulated/0/", "Internal storage/", 1),
            storage,
            update: update(),
            checking: checking(),
            onupdate: move |channel: Channel| {
                let rt = rt.clone();
                checking.set(true);
                update.set(Some("Checking…".into()));
                spawn(async move {
                    update.set(Some(match rt.check_update(channel).await {
                        Ok(o) if o.updated => format!("Downloaded yt-dlp {}. Restart the app to use it.", o.version),
                        Ok(o) => format!("yt-dlp {} is the latest.", o.version),
                        Err(e) => format!("Update failed: {e}"),
                    }));
                    checking.set(false);
                });
            },
            onallow: move |_| platform::request_storage_access(),
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
