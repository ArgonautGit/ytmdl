//! App shell and state. The screens are presentational components in `views`,
//! which `preview` also renders to static HTML with sample data.

mod icons;
#[cfg(test)]
mod preview;
mod views;

use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use ytmdl_core::{Channel, CollectionKind, Entry, Resolved, SearchSource};
use ytmdl_library::{ART_LARGE, ART_SMALL, Album, Artist, Library, Playlist, PlaylistEntry, Track};

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

    /// Deletes songs from the device, the library and the queue.
    fn delete_tracks(&self, ids: Vec<i64>) {
        let ctx = *self;
        let library = ctx.library.get();
        spawn(async move {
            let result = crate::blocking(move || {
                let (mut deleted, mut failed) = (Vec::new(), None);
                for id in ids {
                    match library.delete_track(id) {
                        Ok(Some(track)) => deleted.push(track),
                        Ok(None) => {}
                        Err(e) => failed = Some(e.to_string()),
                    }
                }
                (deleted, failed)
            })
            .await;
            let (deleted, failed) = result.unwrap_or_else(|e| (Vec::new(), Some(format!("{e:#}"))));
            for track in &deleted {
                ctx.player.remove_track(track.id);
                // Scanning a missing file drops it from MediaStore.
                platform::media_scan(&track.path);
            }
            if !deleted.is_empty() {
                ctx.library.changed();
            }
            match failed {
                Some(e) => ctx.notify(format!("Couldn't delete: {e}")),
                None => ctx.notify(format!("Deleted {}", plural(deleted.len(), "song", "songs"))),
            }
        });
    }

    fn play_next(&self, tracks: &[Track]) {
        self.player.play_next(tracks);
        self.notify(if tracks.len() == 1 { "Playing next".into() } else { format!("{} play next", plural(tracks.len(), "song", "songs")) });
    }

    fn add_to_queue(&self, tracks: &[Track]) {
        self.player.add_to_queue(tracks);
        self.notify(if tracks.len() == 1 { "Added to queue".into() } else { format!("Added {} to queue", plural(tracks.len(), "song", "songs")) });
    }

    fn playlist_tracks(&self, id: i64) -> Vec<Track> {
        let entries = self.library.get().playlist_tracks(id).unwrap_or_default();
        entries.into_iter().map(|e| e.track).collect()
    }
}

/// Pages opened over the tabs (albums, artists) and menus. Each is a browser
/// history entry, so Android's back gesture closes the top one.
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
    Playlist(i64),
    /// The full-screen player, over the tabs.
    NowPlaying,
    /// A menu or dialog over the page below.
    Sheet(Sheet),
}

#[derive(Clone, PartialEq)]
enum Sheet {
    Song { track: Box<Track>, from: From },
    Album { title: String, artist: String },
    /// `open`: from the playlist's own page (closed when it is deleted).
    Playlist { id: i64, open: bool },
    /// Picks a playlist for the songs.
    AddTo { tracks: Vec<i64> },
    /// Names a new playlist (holding `tracks`), or renames playlist `id`.
    Name { id: Option<i64>, name: String, tracks: Vec<i64> },
    /// `leave`: the page the songs were on goes too (a whole album).
    DeleteSongs { ids: Vec<i64>, what: String, leave: bool },
    DeletePlaylist { id: i64, name: String, open: bool },
}

/// Where a song's menu was opened, which decides its items.
#[derive(Clone, PartialEq)]
enum From {
    Library,
    Album,
    Artist(String),
    /// The playlist entry.
    Playlist(i64),
    /// The queue entry's key.
    Queue(String),
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

    /// Swaps the top page for another (a menu for the page it leads to, say),
    /// keeping the history entry.
    fn replace(&self, overlay: Overlay) {
        let (mut stack, mut next) = (self.stack, self.next);
        let id = *next.peek();
        next.set(id + 1);
        let mut stack = stack.write();
        stack.pop();
        stack.push((id, overlay));
    }

    /// Closes the top page.
    fn back(&self) {
        self.pop(1);
    }

    /// Closes the top `n` pages now rather than waiting for popstate, so a
    /// missing history entry can't leave one stuck open.
    fn pop(&self, n: usize) {
        let mut stack = self.stack;
        let depth = stack.peek().len().saturating_sub(n);
        if stack.peek().len() > depth {
            stack.write().truncate(depth);
        }
        document::eval(&format!("const d = history.state?.ytmdl_depth ?? 0; if (d > {depth}) history.go({depth} - d)"));
    }

    fn clear(&self) {
        if !self.stack.peek().is_empty() {
            self.pop(usize::MAX);
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
        key: t.id.to_string(),
        title: t.title.clone(),
        artists: t.artists.join(", "),
        album: t.album.clone(),
        duration_secs: t.duration_secs,
        art: art_src(t.art.as_deref(), ART_SMALL),
        playing,
        mark: None,
    }
}

fn playlist_item(p: &Playlist) -> PlaylistItem {
    PlaylistItem { name: p.name.clone(), tracks: p.tracks, duration_secs: p.duration_secs, art: art_src(p.art.as_deref(), ART_SMALL) }
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

    // Songs deleted or rescanned drop out of the queue's view.
    use_effect(move || {
        library.subscribe();
        player.refresh();
    });
    downloads_notification(queue);

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

/// Keeps the downloads foreground service (and its notification) up while
/// downloads run, so Android doesn't freeze them in the background.
fn downloads_notification(queue: Queue) {
    let summary = use_memo(move || {
        let jobs = queue.jobs();
        let jobs = jobs.read();
        let active: Vec<&Job> = jobs.iter().filter(|j| j.is_active()).collect();
        if active.is_empty() {
            return None;
        }
        let running: Vec<&str> =
            active.iter().filter(|j| j.state == JobState::Downloading).map(|j| j.entry.title.as_str()).collect();
        let text = if running.is_empty() { "Waiting…".to_string() } else { running.join(", ") };
        Some((format!("Downloading {}", plural(active.len(), "song", "songs")), text))
    });
    let mut asked = use_signal(|| false);
    use_effect(move || match summary() {
        Some((title, text)) => {
            if !*asked.peek() {
                asked.set(true);
                platform::downloads::ask_notifications();
            }
            platform::downloads::update(&title, &text);
        }
        None => platform::downloads::stop(),
    });
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
    // Pages under the top page are hidden; menus leave the page below showing.
    let top_page = stack.iter().rposition(|(_, o)| !matches!(o, Overlay::Sheet(_)));
    let pages = stack.into_iter().enumerate().map(|(i, (id, overlay))| {
        let class = match overlay {
            Overlay::Sheet(_) => String::new(),
            _ => format!(
                "overlay{}{}",
                if overlay == Overlay::NowPlaying { " sheet" } else { "" },
                if Some(i) == top_page { "" } else { " inactive" },
            ),
        };
        (id, class, overlay)
    });
    rsx! {
        for (id , class , overlay) in pages {
            match overlay {
                Overlay::Sheet(sheet) => rsx! { SheetScreen { key: "{id}", sheet } },
                overlay => rsx! {
                    div { key: "{id}", class,
                        match overlay {
                            Overlay::RemoteAlbum(open) => rsx! { RemoteAlbumScreen { open } },
                            Overlay::Album { title, artist } => rsx! { LocalAlbumScreen { title, artist } },
                            Overlay::Artist(name) => rsx! { ArtistScreen { name } },
                            Overlay::Playlist(id) => rsx! { PlaylistScreen { id } },
                            Overlay::NowPlaying => rsx! { NowPlayingScreen {} },
                            Overlay::Sheet(_) => rsx! {},
                        }
                    }
                },
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

/// Picking the queue's A-B loop: `Some(None)` waits for A, `Some(Some(key))`
/// has A and waits for B.
type Picking = Option<Option<String>>;

#[component]
fn NowPlayingScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let player = ctx.player;
    // The slider's value while dragged, so ticks don't pull it back.
    let mut dragging = use_signal(|| None::<f64>);
    let mut picking = use_signal(|| None as Picking);
    let Some(track) = player.current() else {
        return rsx! {
            EmptyState { icon: Icon::Music, title: "Nothing is playing", text: "Pick a song in your library." }
        };
    };
    let snapshot = player.snapshot();
    let queue = player.queue();
    let queue_loop = player.queue_loop();
    // Queue positions of the loop's ends (or of the A picked so far).
    let position = |key: &str| queue.iter().position(|e| e.key == key);
    let range = match (&*picking.read(), &queue_loop) {
        (Some(Some(a)), _) => position(a).map(|a| (a, a)),
        (Some(None), _) => None,
        (None, Some(l)) => position(&l.first).zip(position(&l.last)),
        (None, None) => None,
    };
    let songs = queue
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut item = song_item(&e.track, e.index == snapshot.index);
            item.key = e.key.clone();
            item.mark = match range {
                Some((a, b)) if a == b && i == a => Some(if picking.read().is_some() { LoopMark::A } else { LoopMark::AB }),
                Some((a, _)) if i == a => Some(LoopMark::A),
                Some((_, b)) if i == b => Some(LoopMark::B),
                Some((a, b)) if a < i && i < b => Some(LoopMark::Inside),
                _ => None,
            };
            item
        })
        .collect();
    let queue_view = match (&*picking.read(), range) {
        (Some(None), _) => QueueLoopView::PickA,
        (Some(Some(_)), _) => QueueLoopView::PickB,
        (None, Some((a, b))) => QueueLoopView::Looping { first: a + 1, last: b + 1 },
        (None, None) => QueueLoopView::Off,
    };
    let secs = |ms: i64| ms as f64 / 1000.0;
    let entries = queue.clone();
    let menu_entries = queue.clone();
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
            song_loop: player.song_loop().map(|(a, b)| (secs(a), b.map(secs))),
            queue: songs,
            queue_loop: queue_view,
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
            onab: move |_| player.song_loop_step(),
            onskip: move |i: usize| {
                let Some(entry) = entries.get(i) else { return };
                let pick = picking.peek().clone();
                match pick {
                    None => player.skip_to(entry.index),
                    Some(None) => picking.set(Some(Some(entry.key.clone()))),
                    Some(Some(first)) => {
                        picking.set(None);
                        player.set_queue_loop(&first, &entry.key);
                    }
                }
            },
            onmore: move |i: usize| {
                if let Some(e) = menu_entries.get(i) {
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(e.track.clone()), from: From::Queue(e.key.clone()) }));
                }
            },
            onqueueloop: move |_| {
                if picking.peek().is_some() {
                    picking.set(None);
                } else if player.queue_loop().is_some() {
                    player.clear_queue_loop();
                } else {
                    picking.set(Some(None));
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
    let playlists: Memo<Vec<Playlist>> = use_memo(move || {
        lib.subscribe();
        lib.get().playlists().unwrap_or_default()
    });
    let current = ctx.player.current_id();
    let songs = tracks.read().iter().map(|t| song_item(t, current == Some(t.id))).collect();
    rsx! {
        LibraryPage {
            view: view(),
            songs,
            albums: albums.read().iter().map(album_item).collect(),
            artists: artists.read().iter().map(artist_item).collect(),
            playlists: playlists.read().iter().map(playlist_item).collect(),
            onview: move |v| view.set(v),
            onplay: move |i| ctx.player.play(tracks(), i),
            onmore: move |i| {
                let track: Option<Track> = tracks.peek().get(i).cloned();
                if let Some(track) = track {
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(track), from: From::Library }));
                }
            },
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
            onplaylist: move |i| {
                let playlist: Option<Playlist> = playlists.peek().get(i).cloned();
                if let Some(p) = playlist {
                    ctx.nav.push(Overlay::Playlist(p.id));
                }
            },
            onplaylistmore: move |i| {
                let playlist: Option<Playlist> = playlists.peek().get(i).cloned();
                if let Some(p) = playlist {
                    ctx.nav.push(Overlay::Sheet(Sheet::Playlist { id: p.id, open: false }));
                }
            },
            onnewplaylist: move |_| {
                ctx.nav.push(Overlay::Sheet(Sheet::Name { id: None, name: String::new(), tracks: Vec::new() }))
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
    let (t, a) = (title.clone(), artist.clone());
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
            onmore: move |i| {
                let track: Option<Track> = tracks.peek().get(i).cloned();
                if let Some(track) = track {
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(track), from: From::Album }));
                }
            },
            onalbummore: move |_| ctx.nav.push(Overlay::Sheet(Sheet::Album { title: t.clone(), artist: a.clone() })),
        }
    }
}

#[component]
fn PlaylistScreen(id: i64) -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let playlist: Memo<Option<Playlist>> = use_memo(move || {
        lib.subscribe();
        lib.get().playlist(id).ok().flatten()
    });
    let entries: Memo<Vec<PlaylistEntry>> = use_memo(move || {
        lib.subscribe();
        lib.get().playlist_tracks(id).unwrap_or_default()
    });
    let tracks = move || entries.peek().iter().map(|e| e.track.clone()).collect::<Vec<_>>();
    let Some(p) = playlist() else {
        return rsx! {
            BackButtonPage { onback: move |_| ctx.nav.back() }
        };
    };
    let current = ctx.player.current_id();
    let songs = entries
        .read()
        .iter()
        .map(|e| {
            let mut item = song_item(&e.track, current == Some(e.track.id));
            item.key = e.entry_id.to_string();
            item
        })
        .collect();
    rsx! {
        PlaylistPage {
            name: p.name.clone(),
            cover: art_src(p.art.as_deref(), ART_LARGE),
            songs,
            onback: move |_| ctx.nav.back(),
            onplay: move |i| ctx.player.play(tracks(), i),
            onshuffle: move |_| ctx.player.shuffle(tracks()),
            onmore: move |i| {
                let entry: Option<PlaylistEntry> = entries.peek().get(i).cloned();
                if let Some(e) = entry {
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(e.track), from: From::Playlist(e.entry_id) }));
                }
            },
            onplaylistmore: move |_| ctx.nav.push(Overlay::Sheet(Sheet::Playlist { id, open: true })),
        }
    }
}

/// A page whose subject is gone (a deleted playlist).
#[component]
fn BackButtonPage(onback: EventHandler<()>) -> Element {
    rsx! {
        div { class: "album",
            BackButton { onback }
            EmptyState { icon: Icon::Playlist, title: "This playlist is gone", text: "It was deleted." }
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
    let artist = name.clone();
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
            onmore: move |i| {
                let track: Option<Track> = tracks.peek().get(i).cloned();
                if let Some(track) = track {
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(track), from: From::Artist(artist.clone()) }));
                }
            },
        }
    }
}

// ---- menus ----

#[component]
fn SheetScreen(sheet: Sheet) -> Element {
    match sheet {
        Sheet::Song { track, from } => rsx! { SongMenu { track: *track, from } },
        Sheet::Album { title, artist } => rsx! { AlbumMenu { title, artist } },
        Sheet::Playlist { id, open } => rsx! { PlaylistMenu { id, open } },
        Sheet::AddTo { tracks } => rsx! { AddToPlaylist { tracks } },
        Sheet::Name { id, name, tracks } => rsx! { NameDialog { id, name, tracks } },
        Sheet::DeleteSongs { ids, what, leave } => rsx! { DeleteSongsDialog { ids, what, leave } },
        Sheet::DeletePlaylist { id, name, open } => rsx! { DeletePlaylistDialog { id, name, open } },
    }
}

#[derive(Clone)]
enum SongAction {
    PlayNext,
    AddToQueue,
    AddToPlaylist,
    RemoveFromPlaylist(i64),
    RemoveFromQueue(String),
    Album,
    Artist,
    Delete,
}

#[component]
fn SongMenu(track: Track, from: From) -> Element {
    let ctx = use_context::<Ctx>();
    let mut items = Vec::new();
    let mut add = |icon, label: &str, action| items.push((MenuItem::new(icon, label), action));
    if !matches!(from, From::Queue(_)) {
        add(Icon::ListStart, "Play next", SongAction::PlayNext);
        add(Icon::ListEnd, "Add to queue", SongAction::AddToQueue);
    }
    add(Icon::ListPlus, "Add to playlist", SongAction::AddToPlaylist);
    match &from {
        From::Playlist(entry) => add(Icon::CircleMinus, "Remove from playlist", SongAction::RemoveFromPlaylist(*entry)),
        From::Queue(key) => add(Icon::CircleMinus, "Remove from queue", SongAction::RemoveFromQueue(key.clone())),
        _ => {}
    }
    if track.album.is_some() && from != From::Album {
        add(Icon::Disc, "Go to album", SongAction::Album);
    }
    let artist = track.artists.first().cloned();
    if artist.is_some() && !matches!(&from, From::Artist(a) if Some(a) == artist.as_ref()) {
        add(Icon::Person, "Go to artist", SongAction::Artist);
    }
    items.push((MenuItem { danger: true, ..MenuItem::new(Icon::Trash, "Delete from device") }, SongAction::Delete));

    let head = MenuHead {
        title: track.title.clone(),
        sub: dotted_text(&[track.artists.join(", "), track.album.clone().unwrap_or_default()]),
        art: art_src(track.art.as_deref(), ART_SMALL),
        icon: Icon::Music,
    };
    let (menu, actions): (Vec<MenuItem>, Vec<SongAction>) = items.into_iter().unzip();
    rsx! {
        MenuSheet {
            head,
            items: menu,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                let Some(action) = actions.get(i).cloned() else { return };
                let t = track.clone();
                match action {
                    SongAction::PlayNext => {
                        ctx.nav.back();
                        ctx.play_next(&[t]);
                    }
                    SongAction::AddToQueue => {
                        ctx.nav.back();
                        ctx.add_to_queue(&[t]);
                    }
                    SongAction::AddToPlaylist => ctx.nav.replace(Overlay::Sheet(Sheet::AddTo { tracks: vec![t.id] })),
                    SongAction::RemoveFromPlaylist(entry) => {
                        ctx.nav.back();
                        match ctx.library.get().remove_from_playlist(entry) {
                            Ok(()) => ctx.library.changed(),
                            Err(e) => ctx.notify(format!("Couldn't remove it: {e}")),
                        }
                    }
                    SongAction::RemoveFromQueue(key) => {
                        ctx.nav.back();
                        ctx.player.remove_entry(&key);
                    }
                    SongAction::Album => {
                        let title = t.album.unwrap_or_default();
                        ctx.nav.replace(Overlay::Album { title, artist: t.album_artist });
                    }
                    SongAction::Artist => {
                        if let Some(name) = t.artists.into_iter().next() {
                            ctx.nav.replace(Overlay::Artist(name));
                        }
                    }
                    SongAction::Delete => {
                        ctx.nav.replace(Overlay::Sheet(Sheet::DeleteSongs { ids: vec![t.id], what: format!("“{}”", t.title), leave: false }))
                    }
                }
            },
        }
    }
}

/// "a • b", skipping empty parts.
fn dotted_text(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" • ")
}

#[component]
fn AlbumMenu(title: String, artist: String) -> Element {
    let ctx = use_context::<Ctx>();
    let tracks = ctx.library.get().album_tracks(&title, &artist).unwrap_or_default();
    let head = MenuHead {
        title: title.clone(),
        sub: dotted_text(&[artist.clone(), plural(tracks.len(), "song", "songs")]),
        art: art_src(tracks.iter().find_map(|t| t.art.as_deref()), ART_SMALL),
        icon: Icon::Disc,
    };
    let items = vec![
        MenuItem::new(Icon::ListStart, "Play next"),
        MenuItem::new(Icon::ListEnd, "Add to queue"),
        MenuItem::new(Icon::ListPlus, "Add to playlist"),
        MenuItem { danger: true, ..MenuItem::new(Icon::Trash, "Delete album from device") },
    ];
    rsx! {
        MenuSheet {
            head,
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                let ids = tracks.iter().map(|t| t.id).collect();
                match i {
                    0 => {
                        ctx.nav.back();
                        ctx.play_next(&tracks);
                    }
                    1 => {
                        ctx.nav.back();
                        ctx.add_to_queue(&tracks);
                    }
                    2 => ctx.nav.replace(Overlay::Sheet(Sheet::AddTo { tracks: ids })),
                    _ => {
                        let what = format!("“{title}” ({})", plural(tracks.len(), "song", "songs"));
                        ctx.nav.replace(Overlay::Sheet(Sheet::DeleteSongs { ids, what, leave: true }))
                    }
                }
            },
        }
    }
}

#[component]
fn PlaylistMenu(id: i64, open: bool) -> Element {
    let ctx = use_context::<Ctx>();
    let Some(playlist) = ctx.library.get().playlist(id).ok().flatten() else {
        return rsx! {};
    };
    let head = MenuHead {
        title: playlist.name.clone(),
        sub: dotted_text(&["Playlist".into(), plural(playlist.tracks as usize, "song", "songs")]),
        art: art_src(playlist.art.as_deref(), ART_SMALL),
        icon: Icon::Playlist,
    };
    let items = vec![
        MenuItem::new(Icon::ListStart, "Play next"),
        MenuItem::new(Icon::ListEnd, "Add to queue"),
        MenuItem::new(Icon::Pencil, "Rename"),
        MenuItem { danger: true, ..MenuItem::new(Icon::Trash, "Delete playlist") },
    ];
    rsx! {
        MenuSheet {
            head,
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| match i {
                0 => {
                    ctx.nav.back();
                    ctx.play_next(&ctx.playlist_tracks(id));
                }
                1 => {
                    ctx.nav.back();
                    ctx.add_to_queue(&ctx.playlist_tracks(id));
                }
                2 => ctx.nav.replace(Overlay::Sheet(Sheet::Name { id: Some(id), name: playlist.name.clone(), tracks: Vec::new() })),
                _ => ctx.nav.replace(Overlay::Sheet(Sheet::DeletePlaylist { id, name: playlist.name.clone(), open })),
            },
        }
    }
}

#[component]
fn AddToPlaylist(tracks: Vec<i64>) -> Element {
    let ctx = use_context::<Ctx>();
    let playlists = ctx.library.get().playlists().unwrap_or_default();
    let items = std::iter::once(MenuItem::new(Icon::Plus, "New playlist"))
        .chain(playlists.iter().map(|p| MenuItem {
            sub: Some(plural(p.tracks as usize, "song", "songs")),
            ..MenuItem::new(Icon::Playlist, p.name.clone())
        }))
        .collect();
    rsx! {
        MenuSheet {
            head: MenuHead {
                title: "Add to playlist".into(),
                sub: plural(tracks.len(), "song", "songs"),
                art: None,
                icon: Icon::ListPlus,
            },
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                let Some(p) = i.checked_sub(1).and_then(|i| playlists.get(i)) else {
                    ctx.nav.replace(Overlay::Sheet(Sheet::Name { id: None, name: String::new(), tracks: tracks.clone() }));
                    return;
                };
                ctx.nav.back();
                match ctx.library.get().add_to_playlist(p.id, &tracks) {
                    Ok(0) => ctx.notify(format!("Already in {}", p.name)),
                    Ok(_) => {
                        ctx.library.changed();
                        ctx.notify(format!("Added to {}", p.name));
                    }
                    Err(e) => ctx.notify(format!("Couldn't add to {}: {e}", p.name)),
                }
            },
        }
    }
}

/// Names a new playlist, or renames one.
#[component]
fn NameDialog(id: Option<i64>, name: String, tracks: Vec<i64>) -> Element {
    let ctx = use_context::<Ctx>();
    let mut value = use_signal(|| name.clone());
    rsx! {
        Dialog {
            title: if id.is_some() { "Rename playlist" } else { "New playlist" },
            value: value(),
            placeholder: "Name",
            confirm: if id.is_some() { "Rename" } else { "Create" },
            oninput: move |v| value.set(v),
            oncancel: move |_| ctx.nav.back(),
            onconfirm: move |_| {
                let name = value.peek().trim().to_string();
                let library = ctx.library.get();
                ctx.nav.back();
                let result = match id {
                    Some(id) => library.rename_playlist(id, &name).map(|_| None),
                    None => library
                        .create_playlist(&name)
                        .and_then(|id| library.add_to_playlist(id, &tracks))
                        .map(|added| Some(if added > 0 { format!("Added to {name}") } else { format!("Created {name}") })),
                };
                match result {
                    Ok(message) => {
                        ctx.library.changed();
                        if let Some(message) = message {
                            ctx.notify(message);
                        }
                    }
                    Err(e) => ctx.notify(format!("Couldn't save the playlist: {e}")),
                }
            },
        }
    }
}

#[component]
fn DeleteSongsDialog(ids: Vec<i64>, what: String, leave: bool) -> Element {
    let ctx = use_context::<Ctx>();
    rsx! {
        Dialog {
            title: if ids.len() == 1 { "Delete this song?" } else { "Delete these songs?" },
            text: format!("{what} will be removed from this device and from your playlists."),
            confirm: "Delete",
            danger: true,
            oncancel: move |_| ctx.nav.back(),
            onconfirm: move |_| {
                ctx.nav.pop(if leave { 2 } else { 1 });
                ctx.delete_tracks(ids.clone());
            },
        }
    }
}

#[component]
fn DeletePlaylistDialog(id: i64, name: String, open: bool) -> Element {
    let ctx = use_context::<Ctx>();
    rsx! {
        Dialog {
            title: "Delete playlist?",
            text: format!("“{name}” will be deleted. Its songs stay in your library."),
            confirm: "Delete",
            danger: true,
            oncancel: move |_| ctx.nav.back(),
            onconfirm: move |_| {
                ctx.nav.pop(if open { 2 } else { 1 });
                match ctx.library.get().delete_playlist(id) {
                    Ok(()) => ctx.library.changed(),
                    Err(e) => ctx.notify(format!("Couldn't delete the playlist: {e}")),
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
