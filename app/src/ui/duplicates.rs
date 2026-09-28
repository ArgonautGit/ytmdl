//! Songs in the library more than once, from different uploads: delete the
//! extra ones, or keep them all.

use std::collections::HashMap;

use dioxus::prelude::*;
use ytmdl_core::titles::tidy;
use ytmdl_library::{ART_SMALL, Duplicates, Track};

use super::views::{DuplicateGroup, DuplicateSong, DuplicatesPage, duration_text, plural};
use super::{Ctx, Overlay, Sheet, art_src, dotted_text};

#[component]
pub(super) fn DuplicatesScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let loaded = use_resource(move || {
        lib.subscribe();
        lib.subscribe_listens();
        let library = lib.get();
        async move { crate::blocking(move || Ok::<_, ytmdl_library::Error>((library.duplicates()?, library.play_counts()?.tracks))).await }
    });
    let loaded: Option<(Vec<Duplicates>, HashMap<String, u32>)> = match &*loaded.read() {
        Some(Ok(Ok(found))) => Some(found.clone()),
        Some(Ok(Err(e))) => {
            tracing::warn!(target: "ytmdl", "duplicates: {e}");
            Some(Default::default())
        }
        Some(Err(e)) => {
            tracing::warn!(target: "ytmdl", "duplicates: {e:#}");
            Some(Default::default())
        }
        None => None,
    };
    let current = ctx.player.current_id();
    let groups = loaded.as_ref().map(|(found, plays)| found.iter().map(|d| group_view(d, plays, current)).collect());
    let found = loaded.map(|(found, _)| found).unwrap_or_default();
    let (to_play, to_delete) = (found.clone(), found.clone());
    rsx! {
        DuplicatesPage {
            groups,
            onback: move |_| ctx.nav.back(),
            onplay: move |(g, i): (usize, usize)| {
                if let Some(d) = to_play.get(g) {
                    ctx.player.play(d.tracks.clone(), i);
                }
            },
            ondelete: move |(g, i): (usize, usize)| {
                let Some(t) = to_delete.get(g).and_then(|d| d.tracks.get(i)) else { return };
                let sheet = Sheet::DeleteSongs { ids: vec![t.id], what: format!("“{}”", t.title), leave: false };
                ctx.nav.push(Overlay::Sheet(sheet));
            },
            onkeep: move |g: usize| {
                let Some(d) = found.get(g) else { return };
                match ctx.library.get().keep_duplicates(d) {
                    Ok(()) => {
                        ctx.library.changed();
                        ctx.notify(if d.tracks.len() == 2 { "Kept both".into() } else { format!("Kept all {}", d.tracks.len()) });
                    }
                    Err(e) => ctx.notify(format!("Couldn't keep them: {e}")),
                }
            },
        }
    }
}

fn group_view(d: &Duplicates, plays: &HashMap<String, u32>, current: Option<i64>) -> DuplicateGroup {
    // Named as the first of them is once tidied.
    let first = &d.tracks[0];
    let (title, artists) = tidy(&first.title, &first.artists);
    let artist = artists.first().cloned().unwrap_or_default();
    let songs = d.tracks.iter().map(|t| song_view(t, &artist, plays.get(&t.video_id).copied().unwrap_or(0), current)).collect();
    DuplicateGroup { title, artist, songs }
}

/// What tells the songs apart comes first: length and plays, then the album,
/// then the artists when they aren't the group's.
fn song_view(t: &Track, artist: &str, plays: u32, current: Option<i64>) -> DuplicateSong {
    let artists = t.artists.join(", ");
    DuplicateSong {
        key: t.id.to_string(),
        title: t.title.clone(),
        sub: dotted_text(&[
            t.duration_secs.map(duration_text).unwrap_or_default(),
            plural(plays as usize, "play", "plays"),
            t.album.clone().unwrap_or_else(|| "Single".into()),
            if artists == artist { String::new() } else { artists },
        ]),
        art: art_src(t.art.as_deref(), ART_SMALL),
        playing: current == Some(t.id),
    }
}
