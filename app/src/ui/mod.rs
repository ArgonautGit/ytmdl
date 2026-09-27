//! App shell and state. The screens are presentational components in `views`,
//! which `preview` also renders to static HTML with sample data.

mod app_update;
mod icons;
mod licenses;
mod lyrics;
#[cfg(test)]
mod preview;
mod sort;
mod stats;
mod sync;
mod update;
mod views;

use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use ytmdl_core::{Channel, CollectionKind, Entry, Resolved, SearchSource};
use ytmdl_library::{ART_LARGE, ART_SMALL, Album, Artist, Library, PlayCounts, Playlist, PlaylistEntry, Section, Track};

use crate::jobs::{Job, JobState, Queue, Services, entry_from_track};
use crate::library::LibraryHandle;
use crate::platform::{self, Dirs};
use crate::player::{Player, Repeat, Sleep};
use app_update::AppUpdates;
use icons::Icon;
use licenses::{LicenseScreen, LicensesScreen, Notice};
use lyrics::LyricsCache;
pub(crate) use lyrics::lookup_enabled as lyrics_lookup_enabled;
use sort::Sorts;
use stats::StatsScreen;
use sync::SyncState;
use update::Updates;
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
    /// Synced playlists syncing now, or whose last sync failed.
    syncs: Signal<HashMap<i64, SyncState>>,
    tab: Signal<Tab>,
    /// A link shared to the app, for the Search tab to open.
    shared: Signal<Option<String>>,
    /// The sort of each library tab.
    sorts: Signal<Sorts>,
    updates: Signal<Updates>,
    /// Builds of the app itself.
    app_updates: Signal<AppUpdates>,
    /// Songs play at an even loudness (ReplayGain).
    normalize: Signal<bool>,
    /// Lyrics read or looked up this run, by track id.
    lyrics: Signal<LyricsCache>,
    /// Now Playing shows the lyrics instead of the cover.
    lyrics_open: Signal<bool>,
    /// Songs without lyrics are looked up on LRCLIB.
    lyrics_lookup: Signal<bool>,
}

/// The Settings switch for even loudness ("0" is off).
const NORMALIZE: &str = "normalize";

impl Ctx {
    fn set_normalize(&self, on: bool) {
        if let Err(e) = self.library.get().set_setting(NORMALIZE, if on { "1" } else { "0" }) {
            self.notify(format!("Couldn't save the setting: {e}"));
            return;
        }
        let mut normalize = self.normalize;
        normalize.set(on);
    }

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

    /// Starts `entry`, or retries its failed or cancelled job.
    fn download(&self, entry: Entry) {
        self.download_entry(entry, true);
    }

    /// Starts `entry` unless it is in the library or has a job running (or,
    /// without `retry`, a failed or cancelled one); returns whether it started.
    fn download_entry(&self, entry: Entry, retry: bool) -> bool {
        let Some(svc) = self.services() else { return false };
        if self.owned.peek().contains(&entry.id) {
            return false;
        }
        let previous = self.queue.jobs().peek().iter().rev().find(|j| j.entry.id == entry.id).map(|j| (j.id, j.state.clone()));
        match previous {
            None | Some((_, JobState::Done { .. })) => self.queue.start(svc, entry),
            Some((id, JobState::Failed(_) | JobState::Cancelled)) if retry => self.queue.retry(svc, id),
            Some(_) => return false,
        }
        true
    }

    /// A song shared to the app downloads straight away.
    fn download_shared(&self, entry: Entry) {
        let title = entry.title.clone();
        if self.owned.peek().contains(&entry.id) {
            self.notify(format!("“{title}” is already in your library"));
        } else if self.download_entry(entry, true) {
            self.notify(format!("Downloading “{title}”"));
        } else {
            self.notify(format!("“{title}” is already downloading"));
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
    Stats,
    /// Open-source licenses, from Settings.
    Licenses,
    License(Notice),
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
    /// How a library tab is sorted.
    Sort(LibraryView),
    Sleep,
    /// A song's saved A-B sections.
    Sections { video_id: String, title: String },
    Section(Section),
    /// Names a new section (`id` None) of the song, or renames one.
    SectionName { id: Option<i64>, name: String, video_id: String, a: i64, b: i64 },
}

/// Where a song's menu was opened, which decides its items.
#[derive(Clone, PartialEq)]
enum From {
    Library,
    Album,
    Artist(String),
    /// A synced playlist's songs follow YouTube, so they can't be removed.
    Playlist { entry: i64, synced: bool },
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
    PlaylistItem {
        name: p.name.clone(),
        tracks: p.tracks,
        duration_secs: p.duration_secs,
        art: art_src(p.art.as_deref(), ART_SMALL),
        synced: p.is_synced(),
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
    let library = use_hook(|| LibraryHandle::new(setup.library.clone(), setup.dirs.data.join("listens.log")));
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
    let syncs = use_signal(HashMap::new);
    let mut tab = use_signal(|| Tab::Library);
    let shared = use_signal(|| None);
    let sorts = use_signal(|| Sorts::load(&setup.library));
    let updates = use_signal(|| Updates::load(&setup.library));
    let app_updates = use_signal(|| AppUpdates::load(&setup.library, setup.dirs.cache.join("app-update")));
    let normalize = use_signal(|| setup.library.setting(NORMALIZE).ok().flatten().as_deref() != Some("0"));
    let lyrics = use_signal(LyricsCache::new);
    let lyrics_open = use_signal(|| false);
    let lyrics_lookup = use_signal(|| lyrics::lookup_enabled(&setup.library));
    let ctx = use_context_provider(|| Ctx {
        boot,
        queue,
        library,
        owned,
        player,
        nav,
        storage,
        toast,
        syncs,
        tab,
        shared,
        sorts,
        updates,
        app_updates,
        normalize,
        lyrics,
        lyrics_open,
        lyrics_lookup,
    });

    let dirs = setup.dirs.clone();
    use_hook(move || spawn(crate::start(boot, dirs, queue)));
    use_hook(move || watch_storage_access(storage));
    serve_art(library);
    // Index new and deleted files at start, and again once the shared folder is
    // readable; then measure the loudness of songs that have none yet.
    let dirs = setup.dirs.clone();
    use_effect(move || {
        let _ = storage();
        let normalize = normalize();
        let dirs = dirs.music_dirs();
        spawn(async move {
            library.scan(dirs).await;
            if normalize {
                library.measure_loudness().await;
            }
        });
    });
    use_effect(move || player.set_normalize(normalize()));

    // Songs deleted or rescanned drop out of the queue's view.
    use_effect(move || {
        library.subscribe();
        player.refresh();
    });
    downloads_notification(queue);
    use_background_work(ctx);
    use_shared_links(ctx);
    app_update::use_install_results(ctx);
    app_update::use_updated_notice(ctx);

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

/// What runs once the downloader is up, whenever the app comes back to the
/// screen, and every ten minutes while it is on screen: syncing playlists
/// whose last sync is old, the daily yt-dlp and app update checks, and reading
/// the player's listening log (also whenever the song changes).
fn use_background_work(ctx: Ctx) {
    use_effect(move || {
        if matches!(*ctx.boot.read(), Boot::Ready(_)) {
            ctx.sync_stale();
            ctx.auto_update_ytdlp();
            ctx.auto_update_app();
        }
    });
    use_hook(move || {
        spawn(async move {
            let mut wake = document::eval(
                "document.addEventListener('visibilitychange', () => { \
                     if (document.visibilityState === 'visible') dioxus.send(true); \
                 }); \
                 while (true) { \
                     await new Promise(r => setTimeout(r, 600000)); \
                     if (document.visibilityState === 'visible') dioxus.send(true); \
                 }",
            );
            while wake.recv::<bool>().await.is_ok() {
                spawn(ctx.library.import_listens());
                if ctx.services().is_some() {
                    ctx.sync_stale();
                    ctx.auto_update_ytdlp();
                    ctx.auto_update_app();
                }
            }
        })
    });
    let song = use_memo(move || ctx.player.current_key());
    use_effect(move || {
        let _ = song();
        spawn(ctx.library.import_listens());
    });
}

/// Links shared to the app open in the Search tab.
fn use_shared_links(ctx: Ctx) {
    use_hook(move || {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        platform::share::listen(tx);
        spawn(async move {
            while let Some(text) = rx.recv().await {
                match ytmdl_core::shared_link(&text) {
                    Some(url) => {
                        let (mut tab, mut shared) = (ctx.tab, ctx.shared);
                        ctx.nav.clear();
                        tab.set(Tab::Search);
                        shared.set(Some(url));
                    }
                    None => ctx.notify("What was shared has no link to open".into()),
                }
            }
        })
    });
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
                            Overlay::Stats => rsx! { StatsScreen {} },
                            Overlay::Licenses => rsx! { LicensesScreen {} },
                            Overlay::License(notice) => rsx! { LicenseScreen { notice } },
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

/// Drag to reorder the queue; reports moves through `window.ytmdlQueueMoved`.
const QUEUE_DRAG_JS: &str = include_str!("../../assets/queue-drag.js");

#[component]
fn NowPlayingScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let player = ctx.player;
    let lib = ctx.library;
    // The slider's value while dragged, so ticks don't pull it back.
    let mut dragging = use_signal(|| None::<f64>);
    let mut picking = use_signal(|| None as Picking);
    let video = use_memo(move || player.current().map(|t| t.video_id));
    let sections: Memo<Vec<Section>> = use_memo(move || {
        lib.subscribe();
        video().map(|v| lib.get().sections(&v).unwrap_or_default()).unwrap_or_default()
    });
    use_hook(move || {
        spawn(async move {
            let script = [QUEUE_DRAG_JS, "window.ytmdlQueueMoved = (from, to) => dioxus.send([from, to]); await new Promise(() => {});"];
            let mut moves = document::eval(&script.join("\n"));
            while let Ok((from, to)) = moves.recv::<(usize, usize)>().await {
                player.move_entry(from, to);
            }
        })
    });
    // The lyrics of the song playing, while they are open.
    let lyrics_track = use_memo(move || player.current().filter(|_| (ctx.lyrics_open)()));
    use_effect(move || {
        if let Some(track) = lyrics_track() {
            ctx.load_lyrics(&track);
        }
    });
    let lyrics_view = use_memo(move || {
        let track = lyrics_track()?;
        let position_ms = (player.position_secs() * 1000.0) as i64;
        Some(ctx.lyrics.read().get(&track.id).map_or(LyricsView::Loading, |s| s.view(position_ms)))
    });
    // Keep the line being sung in the middle of the lyrics box.
    let lyrics_line = use_memo(move || match lyrics_view() {
        Some(LyricsView::Synced { current, .. }) => current,
        _ => None,
    });
    use_effect(move || {
        if lyrics_line().is_some() {
            document::eval(
                "const box = document.querySelector('.lyrics'); \
                 const line = box && box.querySelector('.line.current'); \
                 if (line) box.scrollTo({ top: line.offsetTop - (box.clientHeight - line.offsetHeight) / 2, behavior: 'smooth' });",
            );
        }
    });
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

    // The loop, if it is one of the song's saved sections.
    let song_loop = player.song_loop();
    let looping = song_loop.and_then(|(a, b)| Some((a, b?)));
    let near = |x: i64, y: i64| (x - y).abs() < 50;
    let active = looping.and_then(|(a, b)| sections.read().iter().position(|s| near(s.a_ms, a) && near(s.b_ms, b)));
    let chips = sections
        .read()
        .iter()
        .enumerate()
        .map(|(i, s)| SectionChip { name: s.name.clone(), times: section_times(s.a_ms, s.b_ms), active: active == Some(i) })
        .collect();
    let sleep = player.sleep().map(|s| match s {
        Sleep::EndOfSong => "End of song".to_string(),
        Sleep::At(at) => time_left(at - unix_ms()),
    });
    let (video_id, title) = (track.video_id.clone(), track.title.clone());
    let section_count = sections.read().len();
    let lyrics_lines: Vec<i64> = match ctx.lyrics.read().get(&track.id) {
        Some(lyrics::LyricsState::Found(lines)) => lines.iter().filter_map(|l| l.at_ms).collect(),
        _ => Vec::new(),
    };
    let search_track = track.clone();
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
            song_loop: song_loop.map(|(a, b)| (secs(a), b.map(secs))),
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
            sleep,
            onsleep: move |_| ctx.nav.push(Overlay::Sheet(Sheet::Sleep)),
            sections: chips,
            can_save: looping.is_some() && active.is_none(),
            onsection: move |i: usize| {
                let Some(s) = sections.peek().get(i).cloned() else { return };
                if active == Some(i) {
                    player.stop_song_loop();
                } else {
                    player.loop_section(s.a_ms, s.b_ms);
                }
            },
            onsavesection: {
                let video_id = video_id.clone();
                move |_| {
                    let Some((a, b)) = looping else { return };
                    let name = format!("Section {}", section_count + 1);
                    let sheet = Sheet::SectionName { id: None, name, video_id: video_id.clone(), a, b };
                    ctx.nav.push(Overlay::Sheet(sheet));
                }
            },
            oneditsections: move |_| {
                ctx.nav.push(Overlay::Sheet(Sheet::Sections { video_id: video_id.clone(), title: title.clone() }))
            },
            lyrics: lyrics_view(),
            onlyrics: move |_| {
                let mut open = ctx.lyrics_open;
                open.toggle();
            },
            onlyricsline: move |i: usize| {
                if let Some(&at) = lyrics_lines.get(i) {
                    player.seek(at as f64 / 1000.0);
                }
            },
            onlyricssearch: move |_| ctx.search_lyrics(search_track.clone()),
        }
    }
}

/// "0:32–1:05"
fn section_times(a_ms: i64, b_ms: i64) -> String {
    format!("{}–{}", duration_text(a_ms as f64 / 1000.0), duration_text(b_ms as f64 / 1000.0))
}

/// The sleep timer's time left: "1 hr 5 min", "23 min", "1 min".
fn time_left(ms: i64) -> String {
    let minutes = (ms.max(0) + 59_999) / 60_000;
    match minutes {
        0..60 => format!("{} min", minutes.max(1)),
        _ if minutes % 60 == 0 => format!("{} hr", minutes / 60),
        _ => format!("{} hr {} min", minutes / 60, minutes % 60),
    }
}

/// Now, in Unix ms (the sleep timer's clock).
fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

// ---- library ----

#[component]
fn LibraryScreen(onsearch: EventHandler<()>) -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let sorts = ctx.sorts;
    let mut view = use_signal(|| LibraryView::Songs);
    // The search field's text while it is open.
    let mut query = use_signal(|| None::<String>);
    let tracks = use_memo(move || {
        lib.subscribe();
        lib.get().tracks().unwrap_or_default()
    });
    let all_albums: Memo<Vec<Album>> = use_memo(move || {
        lib.subscribe();
        lib.get().albums().unwrap_or_default()
    });
    let all_artists: Memo<Vec<Artist>> = use_memo(move || {
        lib.subscribe();
        lib.get().artists().unwrap_or_default()
    });
    let all_playlists: Memo<Vec<Playlist>> = use_memo(move || {
        lib.subscribe();
        lib.get().playlists().unwrap_or_default()
    });
    // Only needed (and only read) while a tab sorts by plays.
    let plays: Memo<PlayCounts> = use_memo(move || {
        if !sorts.read().by_plays() {
            return PlayCounts::default();
        }
        lib.subscribe();
        lib.subscribe_listens();
        lib.get().play_counts().unwrap_or_default()
    });
    let q = move || query.read().clone().unwrap_or_default();
    let songs_shown =
        use_memo(move || sort::songs(&tracks.read(), sorts.read().get(LibraryView::Songs), &plays.read(), &q()));
    let albums =
        use_memo(move || sort::albums(&all_albums.read(), sorts.read().get(LibraryView::Albums), &plays.read(), &q()));
    let artists =
        use_memo(move || sort::artists(&all_artists.read(), sorts.read().get(LibraryView::Artists), &plays.read(), &q()));
    let playlists =
        use_memo(move || sort::playlists(&all_playlists.read(), sorts.read().get(LibraryView::Playlists), &q()));
    let current = ctx.player.current_id();
    let songs = songs_shown.read().iter().map(|t| song_item(t, current == Some(t.id))).collect();
    rsx! {
        LibraryPage {
            view: view(),
            songs,
            albums: albums.read().iter().map(album_item).collect(),
            artists: artists.read().iter().map(artist_item).collect(),
            playlists: playlists.read().iter().map(playlist_item).collect(),
            empty: tracks.read().is_empty(),
            query: query(),
            sort: sorts.read().get(view()),
            onview: move |v| view.set(v),
            onplay: move |i| ctx.player.play(songs_shown(), i),
            onmore: move |i| {
                let track: Option<Track> = songs_shown.peek().get(i).cloned();
                if let Some(track) = track {
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(track), from: From::Library }));
                }
            },
            onshuffle: move |_| ctx.player.shuffle(songs_shown()),
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
            onfind: move |_| query.set(Some(String::new())),
            onfindclose: move |_| query.set(None),
            onquery: move |q| query.set(Some(q)),
            onsort: move |_| ctx.nav.push(Overlay::Sheet(Sheet::Sort(view()))),
            onstats: move |_| ctx.nav.push(Overlay::Stats),
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
    let synced = p.is_synced();
    let sync = synced.then(|| match ctx.syncs.read().get(&id) {
        Some(SyncState::Running) => SyncView::Syncing,
        Some(SyncState::Failed(e)) => SyncView::Failed(e.clone()),
        None => SyncView::Synced {
            ago: p.synced_at.map_or_else(|| "never".into(), |t| sync::ago(sync::unix_now() - t)),
            pending: p.wanted.saturating_sub(p.tracks),
        },
    });
    rsx! {
        PlaylistPage {
            name: p.name.clone(),
            cover: art_src(p.art.as_deref(), ART_LARGE),
            songs,
            sync,
            onback: move |_| ctx.nav.back(),
            onplay: move |i| ctx.player.play(tracks(), i),
            onshuffle: move |_| ctx.player.shuffle(tracks()),
            onmore: move |i| {
                let entry: Option<PlaylistEntry> = entries.peek().get(i).cloned();
                if let Some(e) = entry {
                    let from = From::Playlist { entry: e.entry_id, synced };
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(e.track), from }));
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
        Sheet::Sort(view) => rsx! { SortMenu { view } },
        Sheet::Sleep => rsx! { SleepMenu {} },
        Sheet::Sections { video_id, title } => rsx! { SectionsMenu { video_id, title } },
        Sheet::Section(section) => rsx! { SectionMenu { section } },
        Sheet::SectionName { id, name, video_id, a, b } => rsx! { SectionNameDialog { id, name, video_id, a, b } },
    }
}

#[component]
fn SortMenu(view: LibraryView) -> Element {
    let ctx = use_context::<Ctx>();
    let current = ctx.sorts.read().get(view);
    let keys = view.sorts();
    let tab = match view {
        LibraryView::Songs => "songs",
        LibraryView::Albums => "albums",
        LibraryView::Artists => "artists",
        LibraryView::Playlists => "playlists",
    };
    rsx! {
        MenuSheet {
            head: MenuHead { title: "Sort by".into(), sub: format!("Your {tab}"), art: None, icon: Icon::Sort },
            items: keys.iter().map(|k| MenuItem::choice(k.label(), *k == current)).collect(),
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                ctx.nav.back();
                let Some(&key) = keys.get(i) else { return };
                let mut sorts = ctx.sorts;
                sorts.write().set(view, key);
                sorts.peek().save(&ctx.library.get());
            },
        }
    }
}

/// Sleep timer lengths, in minutes.
const SLEEP_MINUTES: [i64; 5] = [5, 15, 30, 45, 60];

#[derive(Clone, Copy)]
enum SleepChoice {
    Off,
    Minutes(i64),
    EndOfSong,
}

#[component]
fn SleepMenu() -> Element {
    let ctx = use_context::<Ctx>();
    let current = ctx.player.sleep();
    let sub = match current {
        None => "Pause the music later".to_string(),
        Some(Sleep::EndOfSong) => "Pausing at the end of this song".to_string(),
        Some(Sleep::At(at)) => format!("Pausing in {}", time_left(at - unix_ms())),
    };
    let mut choices: Vec<(MenuItem, SleepChoice)> = Vec::new();
    if current.is_some() {
        choices.push((MenuItem::new(Icon::Close, "Turn off"), SleepChoice::Off));
    }
    for m in SLEEP_MINUTES {
        let label = if m == 60 { "1 hour".to_string() } else { format!("{m} minutes") };
        choices.push((MenuItem::new(Icon::Moon, label), SleepChoice::Minutes(m)));
    }
    let end = MenuItem { checked: current == Some(Sleep::EndOfSong), ..MenuItem::new(Icon::Music, "End of song") };
    choices.push((end, SleepChoice::EndOfSong));
    let (items, picks): (Vec<MenuItem>, Vec<SleepChoice>) = choices.into_iter().unzip();
    rsx! {
        MenuSheet {
            head: MenuHead { title: "Sleep timer".into(), sub, art: None, icon: Icon::Moon },
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                ctx.nav.back();
                let (sleep, message) = match picks.get(i) {
                    Some(SleepChoice::Off) => (None, "Sleep timer off".to_string()),
                    Some(&SleepChoice::Minutes(m)) => {
                        (Some(Sleep::At(unix_ms() + m * 60_000)), format!("Music pauses in {}", time_left(m * 60_000)))
                    }
                    Some(SleepChoice::EndOfSong) => (Some(Sleep::EndOfSong), "Music pauses at the end of this song".into()),
                    None => return,
                };
                ctx.player.set_sleep(sleep);
                ctx.notify(message);
            },
        }
    }
}

/// A song's saved sections; picking one offers to rename or delete it.
#[component]
fn SectionsMenu(video_id: String, title: String) -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let v = video_id.clone();
    let sections = use_memo(move || {
        lib.subscribe();
        lib.get().sections(&v).unwrap_or_default()
    });
    let items = sections
        .read()
        .iter()
        .map(|s| MenuItem { sub: Some(section_times(s.a_ms, s.b_ms)), ..MenuItem::new(Icon::Bookmark, s.name.clone()) })
        .collect();
    rsx! {
        MenuSheet {
            head: MenuHead { title: "Saved sections".into(), sub: title, art: None, icon: Icon::Bookmark },
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                if let Some(s) = sections.peek().get(i).cloned() {
                    ctx.nav.replace(Overlay::Sheet(Sheet::Section(s)));
                }
            },
        }
    }
}

#[component]
fn SectionMenu(section: Section) -> Element {
    let ctx = use_context::<Ctx>();
    let items = vec![
        MenuItem::new(Icon::Repeat, "Loop it"),
        MenuItem::new(Icon::Pencil, "Rename"),
        MenuItem { danger: true, ..MenuItem::new(Icon::Trash, "Delete") },
    ];
    let head = MenuHead {
        title: section.name.clone(),
        sub: section_times(section.a_ms, section.b_ms),
        art: None,
        icon: Icon::Bookmark,
    };
    rsx! {
        MenuSheet {
            head,
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                let s = section.clone();
                match i {
                    0 => {
                        ctx.nav.back();
                        if ctx.player.current().is_some_and(|t| t.video_id == s.video_id) {
                            ctx.player.loop_section(s.a_ms, s.b_ms);
                        }
                    }
                    1 => {
                        let sheet = Sheet::SectionName { id: Some(s.id), name: s.name, video_id: s.video_id, a: s.a_ms, b: s.b_ms };
                        ctx.nav.replace(Overlay::Sheet(sheet));
                    }
                    _ => {
                        ctx.nav.back();
                        match ctx.library.get().delete_section(s.id) {
                            Ok(()) => {
                                ctx.library.changed();
                                ctx.notify(format!("Deleted “{}”", s.name));
                            }
                            Err(e) => ctx.notify(format!("Couldn't delete it: {e}")),
                        }
                    }
                }
            },
        }
    }
}

/// Names a new section of a song (the loop from `a` to `b` ms), or renames one.
#[component]
fn SectionNameDialog(id: Option<i64>, name: String, video_id: String, a: i64, b: i64) -> Element {
    let ctx = use_context::<Ctx>();
    let mut value = use_signal(|| name.clone());
    rsx! {
        Dialog {
            title: if id.is_some() { "Rename section" } else { "Save section" },
            text: section_times(a, b),
            value: value(),
            placeholder: "Name",
            confirm: if id.is_some() { "Rename" } else { "Save" },
            oninput: move |v| value.set(v),
            oncancel: move |_| ctx.nav.back(),
            onconfirm: move |_| {
                let name = value.peek().trim().to_string();
                let library = ctx.library.get();
                ctx.nav.back();
                let result = match id {
                    Some(id) => library.rename_section(id, &name),
                    None => library.add_section(&video_id, &name, a, b).map(|_| ()),
                };
                match result {
                    Ok(()) => ctx.library.changed(),
                    Err(e) => ctx.notify(format!("Couldn't save the section: {e}")),
                }
            },
        }
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
        From::Playlist { entry, synced: false } => {
            add(Icon::CircleMinus, "Remove from playlist", SongAction::RemoveFromPlaylist(*entry))
        }
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

#[derive(Clone, Copy)]
enum PlaylistAction {
    PlayNext,
    AddToQueue,
    Sync,
    Rename,
    StopSyncing,
    Delete,
}

#[component]
fn PlaylistMenu(id: i64, open: bool) -> Element {
    let ctx = use_context::<Ctx>();
    let Some(playlist) = ctx.library.get().playlist(id).ok().flatten() else {
        return rsx! {};
    };
    let synced = playlist.is_synced();
    let head = MenuHead {
        title: playlist.name.clone(),
        sub: dotted_text(&[
            if synced { "Synced playlist" } else { "Playlist" }.into(),
            plural(playlist.tracks as usize, "song", "songs"),
        ]),
        art: art_src(playlist.art.as_deref(), ART_SMALL),
        icon: Icon::Playlist,
    };
    let mut items = vec![
        (MenuItem::new(Icon::ListStart, "Play next"), PlaylistAction::PlayNext),
        (MenuItem::new(Icon::ListEnd, "Add to queue"), PlaylistAction::AddToQueue),
    ];
    if synced {
        items.push((MenuItem::new(Icon::Sync, "Sync now"), PlaylistAction::Sync));
    }
    items.push((MenuItem::new(Icon::Pencil, "Rename"), PlaylistAction::Rename));
    if synced {
        let item = MenuItem { sub: Some("Keep the songs it has now".into()), ..MenuItem::new(Icon::Unlink, "Stop syncing") };
        items.push((item, PlaylistAction::StopSyncing));
    }
    items.push((MenuItem { danger: true, ..MenuItem::new(Icon::Trash, "Delete playlist") }, PlaylistAction::Delete));
    let (menu, actions): (Vec<MenuItem>, Vec<PlaylistAction>) = items.into_iter().unzip();
    rsx! {
        MenuSheet {
            head,
            items: menu,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| match actions.get(i).copied() {
                Some(PlaylistAction::PlayNext) => {
                    ctx.nav.back();
                    ctx.play_next(&ctx.playlist_tracks(id));
                }
                Some(PlaylistAction::AddToQueue) => {
                    ctx.nav.back();
                    ctx.add_to_queue(&ctx.playlist_tracks(id));
                }
                Some(PlaylistAction::Sync) => {
                    ctx.nav.back();
                    ctx.sync_playlist(id, true);
                }
                Some(PlaylistAction::Rename) => {
                    ctx.nav.replace(Overlay::Sheet(Sheet::Name { id: Some(id), name: playlist.name.clone(), tracks: Vec::new() }))
                }
                Some(PlaylistAction::StopSyncing) => {
                    ctx.nav.back();
                    match ctx.library.get().stop_syncing(id) {
                        Ok(()) => {
                            let mut syncs = ctx.syncs;
                            syncs.write().remove(&id);
                            ctx.library.changed();
                            ctx.notify(format!("{} no longer syncs", playlist.name));
                        }
                        Err(e) => ctx.notify(format!("Couldn't change the playlist: {e}")),
                    }
                }
                Some(PlaylistAction::Delete) => {
                    ctx.nav.replace(Overlay::Sheet(Sheet::DeletePlaylist { id, name: playlist.name.clone(), open }))
                }
                None => {}
            },
        }
    }
}

#[component]
fn AddToPlaylist(tracks: Vec<i64>) -> Element {
    let ctx = use_context::<Ctx>();
    // A synced playlist's songs come from YouTube.
    let playlists: Vec<Playlist> = ctx.library.get().playlists().unwrap_or_default().into_iter().filter(|p| !p.is_synced()).collect();
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

    // `shared`: a link shared to the app, whose song (if it is one) downloads.
    let search = use_callback(move |shared: bool| {
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
                    Ok(Resolved::Track(t)) => {
                        let entry = entry_from_track(t);
                        if shared {
                            ctx.download_shared(entry.clone());
                        }
                        found.set(Found::Entries(SearchSource::MusicSongs, vec![entry]));
                    }
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

    // A link shared to the app is looked up once the downloader is up.
    use_effect(move || {
        let ready = matches!(*ctx.boot.read(), Boot::Ready(_));
        if !ready || ctx.shared.read().is_none() {
            return;
        }
        let mut shared = ctx.shared;
        let Some(url) = shared.write().take() else { return };
        query.set(url);
        search.call(true);
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
                search.call(false);
            },
            onclear: move |_| {
                query.set(String::new());
                found.set(Found::Idle);
                *generation.write() += 1;
            },
            onsource: move |s| {
                source.set(s);
                search.call(false);
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
    let lib = ctx.library;
    // Playlists (not albums) are saved as synced playlists.
    let source = (open.header.kind.as_deref() == Some("playlist")).then(|| ytmdl_core::playlist_url(&open.url)).flatten();
    let is_playlist = source.is_some();
    let saved: Memo<Option<i64>> = use_memo(move || {
        lib.subscribe();
        source.as_deref().and_then(|s| lib.get().synced_playlist(s).ok().flatten())
    });
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
        if let Some(cover) = header.peek().cover.clone().filter(|_| !is_playlist) {
            entry.thumbnail = Some(cover);
        }
        entry
    };
    let open_url = open.url.clone();
    let states = track_states(&ctx.queue.jobs().read(), &ctx.owned.read());
    rsx! {
        AlbumPage {
            tracks: match &*tracks.read() {
                None => AlbumTracks::Loading,
                Some(Err(e)) => AlbumTracks::Error(e.clone()),
                Some(Ok(entries)) => AlbumTracks::Loaded(with_states(entries.clone(), &states)),
            },
            header: header(),
            saved: is_playlist.then(|| saved().is_some()),
            onback: move |_| ctx.nav.back(),
            ondownload: move |e| ctx.download(album_track(e)),
            onopensaved: move |_| {
                if let Some(id) = saved() {
                    ctx.nav.replace(Overlay::Playlist(id));
                }
            },
            ondownloadall: move |_| {
                let Some(Ok(entries)) = tracks.peek().clone() else { return };
                if is_playlist {
                    ctx.save_playlist(&open_url, header.peek().title.clone(), entries);
                    return;
                }
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
    let updates = ctx.updates.read().clone();
    let app_updates = ctx.app_updates.read().clone();
    let Some(svc) = ctx.services() else { return rsx! {} };
    let rt = svc.dl.runtime().clone();
    let v = rt.version().clone();
    let about = About {
        app: app_update::version_text(),
        yt_dlp: v.yt_dlp.clone().unwrap_or_else(|| "unknown".into()),
        python: format!("{} ({})", v.python, v.platform),
        openssl: v.openssl.clone(),
    };
    let storage = (ctx.storage)();
    let output = if storage { &svc.output_dir } else { &svc.fallback_output_dir };
    // Rechecked whenever a check finishes.
    let next_version = if updates.checking { None } else { rt.next_version() };
    let app_update = app_update::this_build().filter(|_| platform::app_update::SUPPORTED).map(|build| AppUpdateInfo {
        build,
        ready: app_updates.ready.as_ref().map(|r| r.build),
        busy: app_updates.busy,
        message: app_updates.message.clone(),
        auto: app_updates.auto,
        auto_note: app_updates.note(),
    });
    rsx! {
        SettingsPage {
            about,
            output: output.display().to_string().replacen("/storage/emulated/0/", "Internal storage/", 1),
            storage,
            update: updates.message.clone(),
            checking: updates.checking,
            next_version,
            auto_update: updates.auto,
            auto_note: updates.note(),
            onupdate: move |channel: Channel| ctx.check_ytdlp(channel, true),
            ontoggleauto: move |_| ctx.set_auto_update(!ctx.updates.peek().auto),
            app_update,
            oncheckapp: move |_| ctx.check_app_update(true),
            oninstallapp: move |_| ctx.install_app_update(),
            ontoggleautoapp: move |_| ctx.set_auto_update_app(!ctx.app_updates.peek().auto),
            onallow: move |_| platform::request_storage_access(),
            onlicenses: move |_| ctx.nav.push(Overlay::Licenses),
            normalize: (ctx.normalize)(),
            ontogglenormalize: move |_| ctx.set_normalize(!*ctx.normalize.peek()),
            lyrics_lookup: (ctx.lyrics_lookup)(),
            ontogglelyrics: move |_| ctx.set_lyrics_lookup(!*ctx.lyrics_lookup.peek()),
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
