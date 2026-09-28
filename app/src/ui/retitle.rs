//! Cleaning up the titles of songs downloaded from videos before ytmdl did
//! it for each download (see ytmdl_core::titles).

use std::collections::HashSet;

use dioxus::prelude::*;
use ytmdl_library::{ART_SMALL, Retitle};

use super::views::{RetitleItem, TitlesPage, plural};
use super::{Ctx, art_src, dotted_text};
use crate::platform;

#[component]
pub(super) fn TitlesScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let loaded = use_resource(move || {
        lib.subscribe();
        let library = lib.get();
        async move { crate::blocking(move || library.untidy()).await }
    });
    // Songs unticked, by track id.
    let mut skipped = use_signal(HashSet::<i64>::new);
    let mut working = use_signal(|| false);
    let loaded: Option<Vec<Retitle>> = match &*loaded.read() {
        Some(Ok(Ok(untidy))) => Some(untidy.clone()),
        Some(Ok(Err(e))) => {
            tracing::warn!(target: "ytmdl", "titles: {e}");
            Some(Vec::new())
        }
        Some(Err(e)) => {
            tracing::warn!(target: "ytmdl", "titles: {e:#}");
            Some(Vec::new())
        }
        None => None,
    };
    let items = loaded.as_ref().map(|untidy| {
        untidy
            .iter()
            .map(|r| RetitleItem {
                key: r.track.id.to_string(),
                title: r.title.clone(),
                artists: r.artists.join(", "),
                was: dotted_text(&[r.track.title.clone(), r.track.artists.join(", ")]),
                art: art_src(r.track.art.as_deref(), ART_SMALL),
                on: !skipped.read().contains(&r.track.id),
            })
            .collect()
    });
    let untidy = loaded.unwrap_or_default();
    let ids: Vec<i64> = untidy.iter().map(|r| r.track.id).collect();
    rsx! {
        TitlesPage {
            items,
            working: working(),
            onback: move |_| ctx.nav.back(),
            ontoggle: move |i: usize| {
                let Some(&id) = ids.get(i) else { return };
                let mut skipped = skipped.write();
                if !skipped.remove(&id) {
                    skipped.insert(id);
                }
            },
            onapply: move |_| {
                let picked: Vec<Retitle> = untidy.iter().filter(|r| !skipped.peek().contains(&r.track.id)).cloned().collect();
                // The player may have the queue's files open: they keep their names.
                let queued: HashSet<i64> = ctx.player.queue().iter().map(|e| e.track.id).collect();
                let library = lib.get();
                working.set(true);
                // Not tied to this page: going back doesn't stop it.
                dioxus::core::spawn_forever(async move {
                    let result = crate::blocking(move || {
                        let (mut moved, mut failed) = (Vec::new(), 0);
                        for r in &picked {
                            match library.retitle(r, !queued.contains(&r.track.id)) {
                                Ok(track) => moved.push((r.track.path.clone(), track.path)),
                                Err(e) => {
                                    tracing::warn!(target: "ytmdl", "cleaning up {}: {e}", r.track.path.display());
                                    failed += 1;
                                }
                            }
                        }
                        (moved, failed)
                    })
                    .await;
                    working.set(false);
                    let (moved, failed) = result.unwrap_or_else(|e| {
                        tracing::warn!(target: "ytmdl", "cleaning up titles: {e:#}");
                        (Vec::new(), 1)
                    });
                    for (from, to) in &moved {
                        // Scanning a missing file drops it from MediaStore.
                        if from != to {
                            platform::media_scan(from);
                        }
                        platform::media_scan(to);
                    }
                    if !moved.is_empty() {
                        lib.changed();
                    }
                    let done = format!("Cleaned up {}", plural(moved.len(), "title", "titles"));
                    ctx.notify(match failed {
                        0 => done,
                        n => format!("{done}; {} couldn't be changed", plural(n, "song", "songs")),
                    });
                });
            },
        }
    }
}
