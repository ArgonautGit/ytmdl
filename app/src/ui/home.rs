//! The Home tab: the playlists played lately, what was played lately and most, radios made from those
//! songs, the albums added last, and songs in the library more than once.

use dioxus::prelude::*;
use ytmdl_library::{ART_LARGE, Home, Track};

use super::views::{HomeList, HomePage, HomeView, SongItem};
use super::{Ctx, From, Overlay, Sheet, album_item, art_src, playlist_item, radio_page, song_item};

/// Radios offered, at most.
const RADIOS: usize = 8;

#[component]
pub(super) fn HomeScreen(onsearch: EventHandler<()>) -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let loaded = use_resource(move || {
        lib.subscribe();
        lib.subscribe_listens();
        let library = lib.get();
        async move { crate::blocking(move || Ok::<_, ytmdl_library::Error>((library.home()?, library.duplicates()?.len()))).await }
    });
    let (home, duplicates) = match &*loaded.read() {
        Some(Ok(Ok((home, duplicates)))) => (Some(home.clone()), *duplicates),
        Some(Ok(Err(e))) => {
            tracing::warn!(target: "ytmdl", "home: {e}");
            (Some(Home::default()), 0)
        }
        Some(Err(e)) => {
            tracing::warn!(target: "ytmdl", "home: {e:#}");
            (Some(Home::default()), 0)
        }
        None => (None, 0),
    };
    let current = ctx.player.current_id();
    let empty = ctx.owned.read().is_empty();
    let view = home.as_ref().map(|h| home_view(h, duplicates, empty, current));
    let home = home.unwrap_or_default();
    let radios = radio_seeds(&home);
    let added = home.added.clone();
    let playlists = home.playlists.clone();
    let for_menu = home.clone();
    rsx! {
        HomePage {
            home: view,
            onsearch,
            onshuffle: move |_| ctx.player.shuffle(lib.get().tracks().unwrap_or_default()),
            onstats: move |_| ctx.nav.push(Overlay::Stats),
            onplaylist: move |i: usize| {
                if let Some(p) = playlists.get(i) {
                    ctx.nav.push(Overlay::Playlist(p.id));
                }
            },
            onplay: move |(which, i)| ctx.player.play(songs(&home, which).to_vec(), i),
            onmore: move |(which, i): (HomeList, usize)| {
                if let Some(track) = songs(&for_menu, which).get(i).cloned() {
                    ctx.nav.push(Overlay::Sheet(Sheet::Song { track: Box::new(track), from: From::Library }));
                }
            },
            onradio: move |i: usize| {
                if let Some(t) = radios.get(i)
                    && ctx.online()
                {
                    ctx.nav.push(radio_page(t));
                }
            },
            onalbum: move |i: usize| {
                if let Some(a) = added.get(i) {
                    ctx.nav.push(Overlay::Album { title: a.title.clone(), artist: a.artist.clone() });
                }
            },
            onduplicates: move |_| ctx.nav.push(Overlay::Duplicates),
        }
    }
}

fn songs(home: &Home, which: HomeList) -> &[Track] {
    match which {
        HomeList::Recent => &home.recent,
        HomeList::Top => &home.top,
        HomeList::Forgotten => &home.forgotten,
    }
}

fn home_view(home: &Home, duplicates: usize, empty: bool, current: Option<i64>) -> HomeView {
    let rows = |tracks: &[Track]| tracks.iter().map(|t| song_item(t, current == Some(t.id))).collect();
    // Tiles are half as wide as the screen, so they get the large art.
    let tile = |t: &Track| SongItem { art: art_src(t.art.as_deref(), ART_LARGE), ..song_item(t, current == Some(t.id)) };
    HomeView {
        greeting: greeting(home.hour).into(),
        empty,
        playlists: home.playlists.iter().map(playlist_item).collect(),
        recent: home.recent.iter().map(tile).collect(),
        top: rows(&home.top),
        radios: radio_seeds(home).iter().map(|t| SongItem { playing: false, ..tile(t) }).collect(),
        added: home.added.iter().map(album_item).collect(),
        forgotten: rows(&home.forgotten),
        duplicates,
    }
}

/// The songs whose radios are offered: the most played lately, then the
/// last played.
fn radio_seeds(home: &Home) -> Vec<Track> {
    let mut seeds: Vec<Track> = Vec::new();
    for t in home.top.iter().chain(&home.recent) {
        if seeds.len() == RADIOS {
            break;
        }
        if !seeds.iter().any(|s| s.id == t.id) {
            seeds.push(t.clone());
        }
    }
    seeds
}

fn greeting(hour: u32) -> &'static str {
    match hour {
        5..12 => "Good morning",
        12..18 => "Good afternoon",
        _ => "Good evening",
    }
}
