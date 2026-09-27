//! Sorting and searching the library tabs. The chosen sort of each tab is kept
//! in the library's settings.

use serde::{Deserialize, Serialize};
use ytmdl_library::{Album, Artist, Library, PlayCounts, Playlist, Track};

use super::views::LibraryView;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    Added,
    Title,
    Artist,
    Album,
    Year,
    Name,
    Songs,
    Plays,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Added => "Recently added",
            SortKey::Title => "Title",
            SortKey::Artist => "Artist",
            SortKey::Album => "Album",
            SortKey::Year => "Year",
            SortKey::Name => "Name",
            SortKey::Songs => "Most songs",
            SortKey::Plays => "Most played",
        }
    }
}

impl LibraryView {
    /// The sorts the tab offers; the first is the default.
    pub fn sorts(self) -> &'static [SortKey] {
        use SortKey::*;
        match self {
            LibraryView::Songs => &[Added, Title, Artist, Album, Plays],
            LibraryView::Albums => &[Added, Title, Artist, Year, Plays],
            LibraryView::Artists => &[Name, Songs, Plays],
            LibraryView::Playlists => &[Added, Name],
        }
    }
}

/// The sort of each library tab.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Sorts {
    songs: SortKey,
    albums: SortKey,
    artists: SortKey,
    playlists: SortKey,
}

impl Default for Sorts {
    fn default() -> Self {
        let first = |v: LibraryView| v.sorts()[0];
        Sorts {
            songs: first(LibraryView::Songs),
            albums: first(LibraryView::Albums),
            artists: first(LibraryView::Artists),
            playlists: first(LibraryView::Playlists),
        }
    }
}

const SETTING: &str = "library_sort";

impl Sorts {
    pub fn load(library: &Library) -> Sorts {
        library.setting(SETTING).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, library: &Library) {
        let json = serde_json::to_string(self).expect("plain data");
        if let Err(e) = library.set_setting(SETTING, &json) {
            tracing::warn!(target: "ytmdl", "saving the library sort: {e}");
        }
    }

    pub fn get(&self, view: LibraryView) -> SortKey {
        match view {
            LibraryView::Songs => self.songs,
            LibraryView::Albums => self.albums,
            LibraryView::Artists => self.artists,
            LibraryView::Playlists => self.playlists,
        }
    }

    pub fn set(&mut self, view: LibraryView, key: SortKey) {
        match view {
            LibraryView::Songs => self.songs = key,
            LibraryView::Albums => self.albums = key,
            LibraryView::Artists => self.artists = key,
            LibraryView::Playlists => self.playlists = key,
        }
    }

    /// Whether any tab sorts by plays (so play counts are needed).
    pub fn by_plays(&self) -> bool {
        [self.songs, self.albums, self.artists].contains(&SortKey::Plays)
    }
}

/// Whether every word of `query` is in one of `fields`, ignoring case.
fn matches(query: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    query.to_lowercase().split_whitespace().all(|word| fields.iter().any(|f| f.contains(word)))
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// `tracks` (most recently added first) searched for `query` and sorted by `key`.
pub fn songs(tracks: &[Track], key: SortKey, plays: &PlayCounts, query: &str) -> Vec<Track> {
    let mut out: Vec<Track> = tracks
        .iter()
        .filter(|t| matches(query, &[&t.title, &t.artists.join(" "), t.album.as_deref().unwrap_or("")]))
        .cloned()
        .collect();
    let album_order = |t: &Track| (t.disc_number.unwrap_or(1), t.track_number.unwrap_or(u32::MAX));
    match key {
        SortKey::Title => out.sort_by_cached_key(|t| lower(&t.title)),
        SortKey::Artist => {
            out.sort_by_cached_key(|t| (lower(&t.artists.join(", ")), t.album.as_deref().map(lower), album_order(t)))
        }
        SortKey::Album => out.sort_by_cached_key(|t| {
            (t.album.is_none(), t.album.as_deref().map(lower), lower(&t.album_artist), album_order(t))
        }),
        SortKey::Plays => out.sort_by_key(|t| std::cmp::Reverse(plays.tracks.get(&t.video_id).copied().unwrap_or(0))),
        _ => {}
    }
    out
}

/// `albums` (most recently added to first) searched and sorted.
pub fn albums(albums: &[Album], key: SortKey, plays: &PlayCounts, query: &str) -> Vec<Album> {
    let mut out: Vec<Album> = albums.iter().filter(|a| matches(query, &[&a.title, &a.artist])).cloned().collect();
    match key {
        SortKey::Title => out.sort_by_cached_key(|a| lower(&a.title)),
        SortKey::Artist => out.sort_by_cached_key(|a| (lower(&a.artist), a.year, lower(&a.title))),
        SortKey::Year => out.sort_by_key(|a| std::cmp::Reverse(a.year)),
        SortKey::Plays => out.sort_by_key(|a| {
            let plays = plays.albums.get(&(a.artist.clone(), a.title.clone())).copied().unwrap_or(0);
            std::cmp::Reverse(plays)
        }),
        _ => {}
    }
    out
}

/// `artists` (by name) searched and sorted.
pub fn artists(artists: &[Artist], key: SortKey, plays: &PlayCounts, query: &str) -> Vec<Artist> {
    let mut out: Vec<Artist> = artists.iter().filter(|a| matches(query, &[&a.name])).cloned().collect();
    match key {
        SortKey::Songs => out.sort_by_key(|a| std::cmp::Reverse(a.tracks)),
        SortKey::Plays => out.sort_by_key(|a| std::cmp::Reverse(plays.artists.get(&a.name).copied().unwrap_or(0))),
        _ => {}
    }
    out
}

/// `playlists` (newest first) searched and sorted.
pub fn playlists(playlists: &[Playlist], key: SortKey, query: &str) -> Vec<Playlist> {
    let mut out: Vec<Playlist> = playlists.iter().filter(|p| matches(query, &[&p.name])).cloned().collect();
    if key == SortKey::Name {
        out.sort_by_cached_key(|p| lower(&p.name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64, title: &str, artist: &str, album: Option<&str>, n: Option<u32>) -> Track {
        Track {
            id,
            video_id: format!("v{id}"),
            url: String::new(),
            path: format!("/m/{id}.m4a").into(),
            title: title.into(),
            artists: vec![artist.into()],
            album: album.map(Into::into),
            album_artist: artist.into(),
            track_number: n,
            disc_number: None,
            year: None,
            duration_secs: None,
            art: None,
            added_at: 0,
        }
    }

    #[test]
    fn searches_and_sorts_songs() {
        let tracks = [
            track(1, "zebra", "Band", Some("Record"), Some(2)),
            track(2, "Apple", "Other", None, None),
            track(3, "Mango", "Band", Some("Record"), Some(1)),
        ];
        let plays = PlayCounts { tracks: [("v2".to_string(), 4), ("v3".to_string(), 9)].into(), ..Default::default() };
        let titles = |key, query| songs(&tracks, key, &plays, query).into_iter().map(|t| t.title).collect::<Vec<_>>();
        assert_eq!(titles(SortKey::Added, ""), ["zebra", "Apple", "Mango"]);
        assert_eq!(titles(SortKey::Title, ""), ["Apple", "Mango", "zebra"]);
        assert_eq!(titles(SortKey::Artist, ""), ["Mango", "zebra", "Apple"]);
        assert_eq!(titles(SortKey::Album, ""), ["Mango", "zebra", "Apple"]);
        assert_eq!(titles(SortKey::Plays, ""), ["Mango", "Apple", "zebra"]);
        assert_eq!(titles(SortKey::Added, "band REC"), ["zebra", "Mango"]);
        assert_eq!(titles(SortKey::Added, "app"), ["Apple"]);
        assert!(titles(SortKey::Added, "band apple").is_empty());
    }

    #[test]
    fn keeps_sorts_per_tab() {
        let mut sorts = Sorts::default();
        assert_eq!(sorts.get(LibraryView::Artists), SortKey::Name);
        assert!(!sorts.by_plays());
        sorts.set(LibraryView::Albums, SortKey::Plays);
        assert!(sorts.by_plays());
        let json = serde_json::to_string(&sorts).unwrap();
        assert_eq!(serde_json::from_str::<Sorts>(&json).unwrap(), sorts);
        // Tabs missing from what was saved get their default.
        assert_eq!(serde_json::from_str::<Sorts>(r#"{"songs":"title"}"#).unwrap().get(LibraryView::Playlists), SortKey::Added);
    }
}
