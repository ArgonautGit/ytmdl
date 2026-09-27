//! The listening stats page: what was played over a period, from the
//! player's log (see crates/library/src/listens.rs).

use dioxus::prelude::*;
use ytmdl_library::{ART_LARGE, ART_SMALL, Bucket, Period, Stats};

use super::views::{Bar, RankItem, StatsPage, StatsView, plural};
use super::{Ctx, Overlay, art_src, dotted_text};

#[component]
pub(super) fn StatsScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let mut period = use_signal(|| Period::Month);
    // What the player logged since the app last looked.
    use_hook(move || spawn(lib.import_listens()));
    let stats = use_resource(move || {
        let period = period();
        lib.subscribe();
        lib.subscribe_listens();
        let library = lib.get();
        async move { crate::blocking(move || library.stats(period)).await }
    });
    let loaded: Option<Stats> = match &*stats.read() {
        Some(Ok(Ok(s))) => Some(s.clone()),
        Some(Ok(Err(e))) => {
            tracing::warn!(target: "ytmdl", "listening stats: {e}");
            Some(Stats::default())
        }
        Some(Err(e)) => {
            tracing::warn!(target: "ytmdl", "listening stats: {e:#}");
            Some(Stats::default())
        }
        None => None,
    };
    let view = loaded.as_ref().map(stats_view);
    let top = loaded.clone().unwrap_or_default();
    let (songs, artists, albums) = (top.top_tracks.clone(), top.top_artists.clone(), top.top_albums.clone());
    rsx! {
        StatsPage {
            period: period(),
            stats: view,
            onback: move |_| ctx.nav.back(),
            onperiod: move |p| period.set(p),
            onsong: move |i| ctx.player.play(songs.iter().map(|t| t.track.clone()).collect(), i),
            onartist: move |i: usize| {
                if let Some(a) = artists.get(i) {
                    ctx.nav.push(Overlay::Artist(a.name.clone()));
                }
            },
            onalbum: move |i: usize| {
                if let Some(a) = albums.get(i) {
                    ctx.nav.push(Overlay::Album { title: a.title.clone(), artist: a.artist.clone() });
                }
            },
        }
    }
}

fn stats_view(s: &Stats) -> StatsView {
    let max = s.buckets.iter().map(|b| b.ms).max().unwrap_or(0);
    let n = s.buckets.len();
    let bars = s
        .buckets
        .iter()
        .enumerate()
        .map(|(i, b)| Bar {
            height: if max > 0 { b.ms as f64 / max as f64 } else { 0.0 },
            label: bar_label(b, i, n),
            now: i + 1 == n,
        })
        .collect();
    let per = match s.buckets.first() {
        Some(Bucket { day: Some(_), .. }) => "Per day",
        Some(Bucket { month: Some(_), .. }) => "Per month",
        _ => "Per year",
    };
    let plays = |n: u32| plural(n as usize, "play", "plays");
    StatsView {
        time: listening_time(s.listened_ms),
        plays: s.plays,
        songs: s.songs,
        artists: s.artists,
        chart_title: per.into(),
        chart_note: (max > 0).then(|| format!("Most: {}", listening_time(max))),
        bars,
        top_songs: s
            .top_tracks
            .iter()
            .map(|t| RankItem {
                title: t.track.title.clone(),
                sub: dotted_text(&[t.track.artists.join(", "), plays(t.plays)]),
                art: art_src(t.track.art.as_deref(), ART_SMALL),
            })
            .collect(),
        top_artists: s
            .top_artists
            .iter()
            .map(|a| RankItem {
                title: a.name.clone(),
                sub: dotted_text(&[plays(a.plays), listening_time(a.ms)]),
                art: art_src(a.art.as_deref(), ART_SMALL),
            })
            .collect(),
        top_albums: s
            .top_albums
            .iter()
            .map(|a| RankItem { title: a.title.clone(), sub: plays(a.plays), art: art_src(a.art.as_deref(), ART_LARGE) })
            .collect(),
    }
}

/// Weekday letters for a week, the day of the month once a week for a month,
/// month letters for a year, and years.
fn bar_label(b: &Bucket, i: usize, n: usize) -> String {
    const WEEKDAYS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
    const MONTHS: [&str; 12] = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"];
    match (b.month, b.day, b.weekday) {
        (_, Some(_), Some(w)) if n <= 7 => WEEKDAYS[w as usize % 7].into(),
        // Today and every seventh day before it.
        (_, Some(d), _) => if (n - 1 - i).is_multiple_of(7) { d.to_string() } else { String::new() },
        // Up to two years of months: letters for one, the year at each January for more.
        (Some(m), None, _) if n <= 12 => MONTHS[(m as usize + 11) % 12].into(),
        (Some(m), None, _) => if m == 1 || i == 0 { format!("’{:02}", b.year % 100) } else { String::new() },
        (None, _, _) => b.year.to_string(),
    }
}

/// "0 min", "45 min", "3 hr 20 min", "120 hr".
pub(super) fn listening_time(ms: i64) -> String {
    let minutes = (ms.max(0) + 30_000) / 60_000;
    match minutes {
        0..60 => format!("{minutes} min"),
        60..6000 if minutes % 60 == 0 => format!("{} hr", minutes / 60),
        60..6000 => format!("{} hr {} min", minutes / 60, minutes % 60),
        _ => format!("{} hr", minutes / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(day: u32, weekday: u32) -> Bucket {
        Bucket { year: 2026, month: Some(9), day: Some(day), weekday: Some(weekday), ms: 0 }
    }

    #[test]
    fn labels_bars() {
        assert_eq!(bar_label(&day(27, 0), 6, 7), "S");
        assert_eq!(bar_label(&day(23, 3), 2, 7), "W");
        assert_eq!(bar_label(&day(27, 0), 29, 30), "27");
        assert_eq!(bar_label(&day(20, 0), 22, 30), "20");
        assert_eq!(bar_label(&day(21, 1), 23, 30), "");
        let month = |m| Bucket { year: 2026, month: Some(m), day: None, weekday: None, ms: 0 };
        assert_eq!(bar_label(&month(10), 0, 12), "O");
        assert_eq!(bar_label(&month(1), 5, 20), "’26");
        assert_eq!(bar_label(&month(3), 7, 20), "");
        assert_eq!(bar_label(&Bucket { month: None, ..month(1) }, 0, 3), "2026");
    }

    #[test]
    fn formats_listening_time() {
        assert_eq!(listening_time(0), "0 min");
        assert_eq!(listening_time(29_000), "0 min");
        assert_eq!(listening_time(45 * 60_000), "45 min");
        assert_eq!(listening_time(60 * 60_000), "1 hr");
        assert_eq!(listening_time(200 * 60_000), "3 hr 20 min");
        assert_eq!(listening_time(7200 * 60_000), "120 hr");
    }
}
