//! Artists' YouTube Music pages, opened from search, from a link, from an
//! artist in the library, or from another artist page. Their lists show as
//! YouTube Music has them: songs and videos to download, albums and playlists
//! to open, similar artists. "Show all" opens a whole list, where "Download
//! all" on the albums downloads every one of them.

use dioxus::prelude::*;
use ytmdl_core::{ArtistPage, CollectionKind, Entry, More, Resolved, SearchSource, SectionKind};

use super::views::*;
use super::{Ctx, OpenAlbum, Overlay, track_states, with_states};
use crate::jobs::entry_from_track;

/// Most entries a "Show all" list loads.
const LIST_LIMIT: usize = 200;

/// An artist page to open, with what is known of it before it loads.
#[derive(Clone, PartialEq)]
pub(super) struct OpenArtist {
    /// The channel id; empty to look the artist up by `name`.
    pub id: String,
    pub name: String,
    pub cover: Option<String>,
}

impl OpenArtist {
    pub fn from_entry(e: &Entry) -> Self {
        OpenArtist { id: e.id.clone(), name: e.title.clone(), cover: e.thumbnail.clone() }
    }

    /// The YouTube Music artist called `name` (an artist in the library).
    pub fn named(name: &str) -> Self {
        OpenArtist { id: String::new(), name: name.to_owned(), cover: None }
    }
}

/// The whole of one list of an artist page.
#[derive(Clone, PartialEq)]
pub(super) struct OpenList {
    pub title: String,
    pub artist: String,
    pub kind: SectionKind,
    /// What the artist page showed of it.
    pub entries: Vec<Entry>,
    /// Where the rest is: browse id and params.
    pub browse: Option<(String, Option<String>)>,
}

impl Ctx {
    /// Downloads every song of `albums` (an artist's discography), one album
    /// after another; songs already here or downloading are left alone.
    fn download_albums(&self, albums: Vec<Entry>) {
        let ctx = *self;
        let Some(svc) = ctx.services() else { return };
        ctx.notify(format!("Looking up {}", plural(albums.len(), "release", "releases")));
        spawn(async move {
            let (mut started, mut failed) = (0, 0);
            for album in albums {
                match svc.dl.resolve_as(&album.url, Some(CollectionKind::Album)).await {
                    Ok(Resolved::Collection { entries, .. }) => {
                        for mut entry in entries {
                            // As on the album's page: the queue shows its square art.
                            entry.thumbnail = album.thumbnail.clone().or(entry.thumbnail);
                            started += usize::from(ctx.download_entry(entry, false));
                        }
                    }
                    Ok(Resolved::Track(t)) => started += usize::from(ctx.download_entry(entry_from_track(t), false)),
                    Err(e) => {
                        tracing::warn!(target: "ytmdl", "loading {}: {e}", album.url);
                        failed += 1;
                    }
                }
            }
            let mut message = match started {
                0 if failed == 0 => "Everything there is downloaded or downloading".to_string(),
                n => format!("Downloading {}", plural(n, "song", "songs")),
            };
            if failed > 0 {
                message += &format!(" (couldn't load {})", plural(failed, "release", "releases"));
            }
            ctx.notify(message);
        });
    }

    /// Opens what an artist page lists: an album, a playlist or an artist.
    fn open_listed(&self, entry: Entry, artist: &str) {
        let overlay = match entry.kind.as_deref() {
            Some("artist") => Overlay::RemoteArtist(OpenArtist::from_entry(&entry)),
            kind => {
                let mut header = AlbumHeader::from_entry(&entry);
                if header.artists.is_empty() && kind != Some("playlist") {
                    header.artists = vec![artist.to_owned()];
                }
                Overlay::RemoteAlbum(OpenAlbum { url: entry.url.clone(), header, tracks: None, radio: None })
            }
        };
        self.nav.push(overlay);
    }
}

/// The artist's page, looking the artist up by name first if need be.
async fn load_page(ctx: Ctx, id: String, name: String) -> Result<ArtistPage, String> {
    let svc = ctx.services().ok_or("The downloader is not running")?;
    let id = if id.is_empty() {
        let found = svc.dl.search(&name, 5, SearchSource::MusicArtists).await.map_err(|e| e.to_string())?;
        let best = found.iter().find(|e| e.title.to_lowercase() == name.to_lowercase()).or(found.first());
        best.map(|e| e.id.clone()).ok_or_else(|| format!("YouTube Music has no artist called {name}"))?
    } else {
        id
    };
    svc.dl.artist(&id).await.map_err(|e| e.to_string())
}

#[component]
pub(super) fn RemoteArtistScreen(open: OpenArtist) -> Element {
    let ctx = use_context::<Ctx>();
    let mut page = use_signal(|| None::<Result<ArtistPage, String>>);
    let (id, name) = (open.id.clone(), open.name.clone());
    let load = use_callback(move |()| {
        let (id, name) = (id.clone(), name.clone());
        page.set(None);
        spawn(async move {
            let loaded = load_page(ctx, id, name.clone()).await.map(|mut p| {
                if p.name.is_empty() {
                    p.name = name;
                }
                p
            });
            page.set(Some(loaded));
        });
    });
    use_hook(move || load.call(()));

    let states = track_states(&ctx.queue.jobs().read(), &ctx.owned.read());
    let (name, cover, audience, about, radio, shelves) = match &*page.read() {
        None => (open.name.clone(), open.cover.clone(), None, None, false, ArtistShelves::Loading),
        Some(Err(e)) => (open.name.clone(), open.cover.clone(), None, None, false, ArtistShelves::Error(e.clone())),
        Some(Ok(p)) => (
            p.name.clone(),
            // The picture it was opened with, so it doesn't change as it loads.
            open.cover.clone().or_else(|| p.art.clone()),
            p.audience.clone(),
            p.description.clone(),
            p.radio.is_some(),
            ArtistShelves::Loaded(
                p.sections
                    .iter()
                    .map(|s| ArtistShelf {
                        title: s.title.clone(),
                        kind: s.kind,
                        entries: with_states(s.entries.clone(), &states, &ctx.keys.read()),
                        more: s.more.is_some(),
                    })
                    .collect(),
            ),
        ),
    };
    let loaded = move || page.peek().as_ref().and_then(|p| p.as_ref().ok()).cloned();
    let picture = cover.clone();
    rsx! {
        RemoteArtistPage {
            name,
            cover,
            audience,
            about,
            shelves,
            radio,
            onback: move |_| ctx.nav.back(),
            onradio: move |_| {
                let Some(p) = loaded() else { return };
                let Some(seed) = p.radio.clone() else { return };
                ctx.nav.push(Overlay::RemoteAlbum(OpenAlbum {
                    url: format!("https://music.youtube.com/channel/{}", p.id),
                    header: AlbumHeader {
                        title: format!("{} radio", p.name),
                        artists: Vec::new(),
                        year: None,
                        kind: Some("radio".into()),
                        cover: picture.clone(),
                    },
                    tracks: None,
                    radio: Some(seed),
                }));
            },
            ondownload: move |e| ctx.download_asked(e),
            onopen: move |e: Entry| {
                let artist = loaded().map(|p| p.name).unwrap_or_default();
                ctx.open_listed(e, &artist);
            },
            onshowall: move |i: usize| {
                let Some(p) = loaded() else { return };
                let Some(s) = p.sections.get(i).cloned() else { return };
                let overlay = match s.more {
                    // All of the artist's songs (or videos) are a playlist.
                    Some(More::Playlist(url)) => Overlay::RemoteAlbum(OpenAlbum {
                        url,
                        header: AlbumHeader {
                            title: format!("{}: {}", p.name, s.title),
                            artists: vec![p.name.clone()],
                            year: None,
                            kind: Some("playlist".into()),
                            cover: s.entries.iter().find_map(|e| e.thumbnail.clone()),
                        },
                        tracks: None,
                        radio: None,
                    }),
                    more => Overlay::RemoteList(OpenList {
                        title: s.title,
                        artist: p.name,
                        kind: s.kind,
                        entries: s.entries,
                        browse: match more {
                            Some(More::Browse { browse_id, params }) => Some((browse_id, params)),
                            _ => None,
                        },
                    }),
                };
                ctx.nav.push(overlay);
            },
            onretry: move |_| load.call(()),
        }
    }
}

#[component]
pub(super) fn RemoteListScreen(open: OpenList) -> Element {
    let ctx = use_context::<Ctx>();
    let mut entries = use_signal(|| open.entries.clone());
    let mut loading = use_signal(|| open.browse.is_some());
    let mut error = use_signal(|| None::<String>);
    let browse = open.browse.clone();
    use_hook(move || {
        let Some((id, params)) = browse else { return };
        let Some(svc) = ctx.services() else {
            loading.set(false);
            return;
        };
        spawn(async move {
            match svc.dl.browse(&id, params.as_deref(), LIST_LIMIT).await {
                Ok(found) if !found.is_empty() => entries.set(found),
                // Nothing listed: keep what the artist page showed.
                Ok(_) => {}
                Err(e) => error.set(Some(e.to_string())),
            }
            loading.set(false);
        });
    });
    let states = track_states(&ctx.queue.jobs().read(), &ctx.owned.read());
    let kind = open.kind;
    let artist = open.artist.clone();
    rsx! {
        RemoteListPage {
            title: open.title.clone(),
            sub: open.artist.clone(),
            kind,
            entries: with_states(entries(), &states, &ctx.keys.read()),
            loading: loading(),
            error: error(),
            onback: move |_| ctx.nav.back(),
            ondownload: move |e| ctx.download_asked(e),
            onopen: move |e| ctx.open_listed(e, &artist),
            ondownloadall: move |_| {
                let listed = entries.peek().clone();
                if kind == SectionKind::Albums {
                    ctx.download_albums(listed);
                    return;
                }
                let states = track_states(&ctx.queue.jobs().peek(), &ctx.owned.peek());
                let todo: Vec<Entry> =
                    listed.into_iter().filter(|e| states.get(&e.id).copied().unwrap_or_default().wants_download()).collect();
                let n = todo.len();
                for e in todo {
                    ctx.download(e);
                }
                if n > 0 {
                    ctx.notify(format!("Downloading {}", plural(n, "song", "songs")));
                }
            },
        }
    }
}
