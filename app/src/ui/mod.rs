//! App shell and state. The screens are presentational components in `views`,
//! which `preview` also renders to static HTML with sample data.

mod icons;
#[cfg(test)]
mod preview;
mod views;

use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use ytmdl_core::{Channel, CollectionKind, Entry, Resolved, SearchSource};
use ytmdl_library::{ART_LARGE, ART_SMALL, Album, Artist, Library, Track};

use crate::jobs::{Job, JobState, Queue, Services, entry_from_track};
use crate::library::LibraryHandle;
use crate::platform::{self, Dirs};
use crate::player::{Player, Repeat};
use icons::Icon;
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
    /// The downloader (Python and yt-dlp); the library works without it.
    boot: Signal<Boot>,
    queue: Queue,
    library: LibraryHandle,
    /// Video ids in the library.
    owned: Memo<HashSet<String>>,
    player: Player,
    nav: Nav,
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

    /// Starts `entry`, or retries its failed or cancelled job; no-op while one is
    /// running, or when the song is already in the library.
    fn download(&self, entry: Entry) {
        let Some(svc) = self.services() else { return };
        if self.owned.peek().contains(&entry.id) {
            return;
        }
        let previous = self.queue.jobs().peek().iter().rev().find(|j| j.entry.id == entry.id).map(|j| (j.id, j.state.clone()));
        match previous {
            None | Some((_, JobState::Done { .. })) => self.queue.start(svc, entry),
            Some((id, JobState::Failed(_) | JobState::Cancelled)) => self.queue.retry(svc, id),
            Some(_) => {}
        }
    }
}

/// Pages opened over the tabs (albums, artists). Each is a browser history entry,
/// so Android's back gesture closes the top one.
#[derive(Clone, Copy)]
struct Nav {
    stack: Signal<Vec<(u64, Overlay)>>,
    next: Signal<u64>,
}

#[derive(Clone, PartialEq)]
enum Overlay {
    /// A YouTube Music album or playlist from search.
    RemoteAlbum(OpenAlbum),
    /// A downloaded album.
    Album { title: String, artist: String },
    Artist(String),
    /// The full-screen player, over the tabs.
    NowPlaying,
}

impl Nav {
    fn new() -> Self {
        let nav = Nav { stack: Signal::new(Vec::new()), next: Signal::new(0) };
        // Back gestures pop history entries; follow them to the depth they reach.
        spawn(async move {
            let mut popped = document::eval(
                "window.addEventListener('popstate', e => dioxus.send(e.state?.ytmdl_depth ?? 0)); \
                 await new Promise(() => {});",
            );
            while let Ok(depth) = popped.recv::<usize>().await {
                let mut stack = nav.stack;
                if stack.peek().len() > depth {
                    stack.write().truncate(depth);
                }
            }
        });
        nav
    }

    fn push(&self, overlay: Overlay) {
        let (mut stack, mut next) = (self.stack, self.next);
        let id = *next.peek();
        next.set(id + 1);
        stack.write().push((id, overlay));
        let depth = stack.peek().len();
        document::eval(&format!("history.pushState({{ ytmdl_depth: {depth} }}, '')"));
    }

    /// Closes the top page now rather than waiting for popstate, so a missing
    /// history entry can't leave it stuck open.
    fn back(&self) {
        let mut stack = self.stack;
        stack.write().pop();
        document::eval("if (history.state?.ytmdl_depth) history.back()");
    }

    fn clear(&self) {
        let mut stack = self.stack;
        if !stack.peek().is_empty() {
            stack.write().clear();
            document::eval("const d = history.state?.ytmdl_depth; if (d) history.go(-d)");
        }
    }
}

/// Latest download state per video id; songs in the library count as done.
fn track_states(jobs: &[Job], owned: &HashSet<String>) -> HashMap<String, TrackState> {
    let mut states: HashMap<String, TrackState> = owned.iter().map(|id| (id.clone(), TrackState::Done)).collect();
    for job in jobs {
        let state = match &job.state {
            JobState::Queued => TrackState::Queued,
            JobState::Downloading => {
                TrackState::Downloading(job.progress.as_ref().and_then(|p| p.fraction()).unwrap_or(0.0))
            }
            JobState::Done { .. } => TrackState::Done,
            JobState::Failed(_) => TrackState::Failed,
            JobState::Cancelled => continue,
        };
        states.insert(job.entry.id.clone(), state);
    }
    states
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

/// Served from the art cache by the `art` asset handler.
fn art_src(key: Option<&str>, size: u32) -> Option<String> {
    key.map(|k| format!("/art/{}", Library::art_name(k, size)))
}

fn song_item(t: &Track, playing: bool) -> SongItem {
    SongItem {
        id: t.id,
        title: t.title.clone(),
        artists: t.artists.join(", "),
        album: t.album.clone(),
        duration_secs: t.duration_secs,
        art: art_src(t.art.as_deref(), ART_SMALL),
        playing,
    }
}

/// Grid tiles are half the screen wide, so they get the large art.
fn album_item(a: &Album) -> AlbumItem {
    AlbumItem { title: a.title.clone(), artist: a.artist.clone(), year: a.year, art: art_src(a.art.as_deref(), ART_LARGE) }
}

fn artist_item(a: &Artist) -> ArtistItem {
    ArtistItem { name: a.name.clone(), tracks: a.tracks, albums: a.albums, art: art_src(a.art.as_deref(), ART_SMALL) }
}

/// Everything the shell needs before it can show anything.
#[derive(Clone)]
struct Setup {
    dirs: Dirs,
    library: Library,
}

impl PartialEq for Setup {
    fn eq(&self, _: &Self) -> bool {
        true // made once per launch
    }
}

fn setup() -> anyhow::Result<Setup> {
    let dirs = platform::dirs()?;
    let library = Library::open(&dirs.data.join("library.db"), &dirs.data.join("art"))?;
    Ok(Setup { dirs, library })
}

#[component]
pub fn App() -> Element {
    let setup = use_hook(|| setup().map_err(|e| format!("{e:#}")));
    rsx! {
        style { {CSS} }
        match setup {
            Ok(setup) => rsx! { Shell { setup } },
            Err(error) => rsx! { div { class: "app", StartupScreen { error } } },
        }
    }
}

#[component]
fn Shell(setup: Setup) -> Element {
    let library = use_hook(|| LibraryHandle::new(setup.library.clone()));
    let boot = use_signal(|| Boot::Starting);
    let queue = use_hook(|| Queue::new(library));
    let owned = use_memo(move || {
        library.subscribe();
        library.get().video_ids().unwrap_or_default()
    });
    let player = use_hook(|| Player::new(library));
    let nav = use_hook(Nav::new);
    let storage = use_signal(platform::has_storage_access);
    let toast = use_signal(|| None);
    let ctx = use_context_provider(|| Ctx { boot, queue, library, owned, player, nav, storage, toast });
    let mut tab = use_signal(|| Tab::Library);

    let dirs = setup.dirs.clone();
    use_hook(move || spawn(crate::start(boot, dirs, queue)));
    use_hook(move || watch_storage_access(storage));
    serve_art(library);
    // Index new and deleted files at start, and again once the shared folder is readable.
    let dirs = setup.dirs.clone();
    use_effect(move || {
        let _ = storage();
        spawn(library.scan(dirs.music_dirs()));
    });

    // Playback errors (a file deleted elsewhere, say) show once each.
    let mut shown_error = use_signal(|| None::<String>);
    use_effect(move || {
        let error = player.snapshot().error;
        if error.is_some() && error != *shown_error.peek() {
            let title = player.current().map(|t| t.title).unwrap_or_default();
            ctx.notify(format!("Can't play {title}: {}", error.clone().unwrap_or_default()));
        }
        shown_error.set(error);
    });

    let active = queue.jobs().read().iter().filter(|j| j.is_active()).count();
    let has_mini = player.current().is_some();
    let paused = !player.snapshot().wants_play();
    rsx! {
        div { class: format!("app{}{}", if has_mini { " has-mini" } else { "" }, if paused { " paused" } else { "" }),
            // Every tab stays mounted (and keeps its scroll position); only one shows.
            Page { visible: tab() == Tab::Library, LibraryScreen { onsearch: move |_| tab.set(Tab::Search) } }
            Page { visible: tab() == Tab::Search, SearchScreen {} }
            Page { visible: tab() == Tab::Downloads, DownloadsScreen {} }
            Page { visible: tab() == Tab::Settings, SettingsScreen {} }
            Overlays {}
            if has_mini {
                MiniPlayerBar {}
            }
            BottomNav {
                tab: tab(),
                active,
                onselect: move |t| {
                    nav.clear();
                    tab.set(t);
                },
            }
            for (id , message) in toast() {
                Toast { key: "{id}", message }
            }
        }
    }
}

/// Serves `/art/<name>` from the cover art cache.
fn serve_art(library: LibraryHandle) {
    #[cfg(any(feature = "desktop", feature = "mobile"))]
    {
        #[cfg(feature = "desktop")]
        use dioxus::desktop::{use_asset_handler, wry::http::Response};
        #[cfg(all(feature = "mobile", not(feature = "desktop")))]
        use dioxus::mobile::{use_asset_handler, wry::http::Response};

        use_asset_handler("art", move |request, responder| {
            let name = request.uri().path().trim_start_matches("/art/");
            let file = library.get().art_file(name).and_then(|p| std::fs::read(p).ok());
            let response = match file {
                Some(bytes) => Response::builder()
                    .header("Content-Type", "image/jpeg")
                    .header("Cache-Control", "max-age=31536000, immutable")
                    .body(bytes),
                None => Response::builder().status(404).body(Vec::new()),
            };
            responder.respond(response.expect("static response parts"));
        });
    }
    #[cfg(not(any(feature = "desktop", feature = "mobile")))]
    let _ = library;
}

#[component]
fn Overlays() -> Element {
    let ctx = use_context::<Ctx>();
    let stack = ctx.nav.stack.read().clone();
    let top = stack.len();
    let pages = stack.into_iter().enumerate().map(|(i, (id, overlay))| {
        let class = format!(
            "overlay{}{}",
            if overlay == Overlay::NowPlaying { " sheet" } else { "" },
            if i + 1 == top { "" } else { " inactive" },
        );
        (id, class, overlay)
    });
    rsx! {
        for (id , class , overlay) in pages {
            div { key: "{id}", class,
                match overlay {
                    Overlay::RemoteAlbum(open) => rsx! { RemoteAlbumScreen { open } },
                    Overlay::Album { title, artist } => rsx! { LocalAlbumScreen { title, artist } },
                    Overlay::Artist(name) => rsx! { ArtistScreen { name } },
                    Overlay::NowPlaying => rsx! { NowPlayingScreen {} },
                }
            }
        }
    }
}

fn now_item(t: &Track) -> NowItem {
    NowItem {
        title: t.title.clone(),
        artists: t.artists.join(", "),
        album: t.album.clone(),
        art: art_src(t.art.as_deref(), ART_SMALL),
        art_large: art_src(t.art.as_deref(), ART_LARGE),
    }
}

#[component]
fn MiniPlayerBar() -> Element {
    let ctx = use_context::<Ctx>();
    let player = ctx.player;
    let Some(track) = player.current() else { return rsx! {} };
    let duration = player.duration_secs();
    let progress = if duration > 0.0 { player.position_secs() / duration } else { 0.0 };
    rsx! {
        MiniPlayer {
            now: now_item(&track),
            progress,
            playing: player.snapshot().wants_play(),
            ontoggle: move |_| player.toggle(),
            onnext: move |_| player.next(),
            onopen: move |_| ctx.nav.push(Overlay::NowPlaying),
        }
    }
}

#[component]
fn NowPlayingScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let player = ctx.player;
    // The slider's value while dragged, so ticks don't pull it back.
    let mut dragging = use_signal(|| None::<f64>);
    let Some(track) = player.current() else {
        return rsx! {
            EmptyState { icon: Icon::Music, title: "Nothing is playing", text: "Pick a song in your library." }
        };
    };
    let snapshot = player.snapshot();
    let queue = player.queue();
    let songs = queue.iter().map(|(i, t)| song_item(t, *i == snapshot.index)).collect();
    rsx! {
        NowPlayingPage {
            now: now_item(&track),
            position: dragging().unwrap_or_else(|| player.position_secs()),
            duration: player.duration_secs(),
            playing: snapshot.wants_play(),
            buffering: snapshot.buffering,
            repeat: match player.repeat() {
                Repeat::Off => RepeatMode::Off,
                Repeat::All => RepeatMode::All,
                Repeat::One => RepeatMode::One,
            },
            queue: songs,
            onclose: move |_| ctx.nav.back(),
            ontoggle: move |_| player.toggle(),
            onnext: move |_| player.next(),
            onprevious: move |_| player.previous(),
            onseeking: move |v| dragging.set(Some(v)),
            onseek: move |v| {
                player.seek(v);
                dragging.set(None);
            },
            onrepeat: move |_| player.cycle_repeat(),
            onskip: move |i: usize| {
                if let Some((index, _)) = queue.get(i) {
                    player.skip_to(*index);
                }
            },
        }
    }
}

// ---- library ----

#[component]
fn LibraryScreen(onsearch: EventHandler<()>) -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let mut view = use_signal(|| LibraryView::Songs);
    let tracks = use_memo(move || {
        lib.subscribe();
        lib.get().tracks().unwrap_or_default()
    });
    let albums: Memo<Vec<Album>> = use_memo(move || {
        lib.subscribe();
        lib.get().albums().unwrap_or_default()
    });
    let artists: Memo<Vec<Artist>> = use_memo(move || {
        lib.subscribe();
        lib.get().artists().unwrap_or_default()
    });
    let current = ctx.player.current_id();
    let songs = tracks.read().iter().map(|t| song_item(t, current == Some(t.id))).collect();
    rsx! {
        LibraryPage {
            view: view(),
            songs,
            albums: albums.read().iter().map(album_item).collect(),
            artists: artists.read().iter().map(artist_item).collect(),
            onview: move |v| view.set(v),
            onplay: move |i| ctx.player.play(tracks(), i),
            onshuffle: move |_| ctx.player.shuffle(tracks()),
            onalbum: move |i| {
                let album: Option<Album> = albums.peek().get(i).cloned();
                if let Some(a) = album {
                    ctx.nav.push(Overlay::Album { title: a.title, artist: a.artist });
                }
            },
            onartist: move |i| {
                let artist: Option<Artist> = artists.peek().get(i).cloned();
                if let Some(a) = artist {
                    ctx.nav.push(Overlay::Artist(a.name));
                }
            },
            onsearch,
        }
    }
}

#[component]
fn LocalAlbumScreen(title: String, artist: String) -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let (t, a) = (title.clone(), artist.clone());
    let tracks = use_memo(move || {
        lib.subscribe();
        lib.get().album_tracks(&t, &a).unwrap_or_default()
    });
    let list = tracks.read();
    let current = ctx.player.current_id();
    let header = AlbumHeader {
        title,
        artists: vec![artist],
        year: list.iter().find_map(|t| t.year),
        kind: None,
        cover: art_src(list.iter().find_map(|t| t.art.as_deref()), ART_LARGE),
    };
    rsx! {
        LocalAlbumPage {
            header,
            songs: list.iter().map(|t| song_item(t, current == Some(t.id))).collect(),
            onback: move |_| ctx.nav.back(),
            onplay: move |i| ctx.player.play(tracks(), i),
            onshuffle: move |_| ctx.player.shuffle(tracks()),
        }
    }
}

#[component]
fn ArtistScreen(name: String) -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let n = name.clone();
    let tracks = use_memo(move || {
        lib.subscribe();
        lib.get().artist_tracks(&n).unwrap_or_default()
    });
    let n = name.clone();
    let albums: Memo<Vec<Album>> = use_memo(move || {
        lib.subscribe();
        let albums = lib.get().albums().unwrap_or_default();
        albums.into_iter().filter(|a| a.artist == n).collect()
    });
    let list = tracks.read();
    let current = ctx.player.current_id();
    rsx! {
        ArtistPage {
            name,
            art: art_src(list.iter().find_map(|t| t.art.as_deref()), ART_LARGE),
            songs: list.iter().map(|t| song_item(t, current == Some(t.id))).collect(),
            albums: albums.read().iter().map(album_item).collect(),
            onback: move |_| ctx.nav.back(),
            onplay: move |i| ctx.player.play(tracks(), i),
            onshuffle: move |_| ctx.player.shuffle(tracks()),
            onalbum: move |i| {
                let album: Option<Album> = albums.peek().get(i).cloned();
                if let Some(a) = album {
                    ctx.nav.push(Overlay::Album { title: a.title, artist: a.artist });
                }
            },
        }
    }
}

// ---- search ----

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
    tracks: Option<Vec<Entry>>,
}

#[component]
fn SearchScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let mut query = use_signal(String::new);
    let mut source = use_signal(|| SearchSource::MusicSongs);
    let mut found = use_signal(|| Found::Idle);
    // Bumped per search so a slow earlier search can't overwrite a newer one.
    let mut generation = use_signal(|| 0u64);

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
                        ctx.nav.push(Overlay::RemoteAlbum(OpenAlbum {
                            url: q,
                            header: AlbumHeader {
                                title,
                                artists: first.map(|e| e.artists.clone()).unwrap_or_default(),
                                year: None,
                                kind: Some(if kind == CollectionKind::Album { "album" } else { "playlist" }.into()),
                                cover: entries.iter().find_map(|e| e.thumbnail.clone()),
                            },
                            tracks: Some(entries),
                        }));
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

    let states = track_states(&ctx.queue.jobs().read(), &ctx.owned.read());
    let results = match (&*ctx.boot.read(), found()) {
        (Boot::Starting, _) => ResultsView::Starting,
        (Boot::Failed(e), _) => ResultsView::Unavailable(e.clone()),
        (_, Found::Idle) => ResultsView::Idle,
        (_, Found::Loading) => ResultsView::Loading,
        (_, Found::Error(e)) => ResultsView::Error(e),
        (_, Found::Entries(SearchSource::MusicAlbums, entries)) => ResultsView::Albums(entries),
        (_, Found::Entries(SearchSource::YouTube, entries)) => ResultsView::Videos(with_states(entries, &states)),
        (_, Found::Entries(SearchSource::MusicSongs, entries)) => ResultsView::Songs(with_states(entries, &states)),
    };
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
                ctx.nav.push(Overlay::RemoteAlbum(OpenAlbum { url: e.url.clone(), header: AlbumHeader::from_entry(&e), tracks: None }))
            },
            onallow: move |_| platform::request_storage_access(),
        }
    }
}

/// A YouTube Music album or playlist: loads its tracks, offers downloads.
#[component]
fn RemoteAlbumScreen(open: OpenAlbum) -> Element {
    let ctx = use_context::<Ctx>();
    let url = open.url.clone();
    let mut header = use_signal(|| open.header.clone());
    let mut tracks = use_signal(|| open.tracks.clone().map(Ok::<_, String>));

    let load = use_callback(move |()| {
        let Some(svc) = ctx.services() else { return };
        let url = url.clone();
        tracks.set(None);
        spawn(async move {
            let result = match svc.dl.resolve(&url).await {
                Ok(Resolved::Collection { entries, .. }) => Ok(entries),
                Ok(Resolved::Track(t)) => Ok(vec![entry_from_track(t)]),
                Err(e) => Err(e.to_string()),
            };
            if let Ok(entries) = &result
                && header.peek().cover.is_none()
            {
                header.write().cover = entries.iter().find_map(|e| e.thumbnail.clone());
            }
            tracks.set(Some(result));
        });
    });
    use_hook(move || {
        if tracks.peek().is_none() {
            load.call(());
        }
    });

    // Album tracks list video thumbnails; the queue shows the album's square art instead.
    let album_track = move |mut entry: Entry| {
        if let Some(cover) = header.peek().cover.clone() {
            entry.thumbnail = Some(cover);
        }
        entry
    };
    let states = track_states(&ctx.queue.jobs().read(), &ctx.owned.read());
    rsx! {
        AlbumPage {
            tracks: match &*tracks.read() {
                None => AlbumTracks::Loading,
                Some(Err(e)) => AlbumTracks::Error(e.clone()),
                Some(Ok(entries)) => AlbumTracks::Loaded(with_states(entries.clone(), &states)),
            },
            header: header(),
            onback: move |_| ctx.nav.back(),
            ondownload: move |e| ctx.download(album_track(e)),
            ondownloadall: move |_| {
                let Some(Ok(entries)) = tracks.peek().clone() else { return };
                let states = track_states(&ctx.queue.jobs().peek(), &ctx.owned.peek());
                let todo: Vec<Entry> = entries
                    .into_iter()
                    .filter(|e| states.get(&e.id).copied().unwrap_or_default().wants_download())
                    .collect();
                let n = todo.len();
                for e in todo {
                    ctx.download(album_track(e));
                }
                if n > 0 {
                    ctx.notify(format!("Downloading {}", plural(n, "song", "songs")));
                }
            },
            onretry: move |_| load.call(()),
        }
    }
}

// ---- downloads and settings ----

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
                    ctx.notify(format!("Cancelled {}", plural(n, "download", "downloads")));
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
