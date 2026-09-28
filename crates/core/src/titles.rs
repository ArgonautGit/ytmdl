//! Song titles as YouTube gives them, tidied. Uploads other than YouTube
//! Music's own songs say what the video is ("Song (Official Video)", "Song |
//! Lyric Video") and, from an artist's channel, who it is by ("Artist -
//! Song"): [`tidy`] keeps the song's title and moves the artist out of it.
//! [`song_key`] names a song whatever upload it came from, to find songs
//! downloaded twice.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Words that say what an upload is. A bracketed part (or a part after " | ",
/// " - ") of these alone, with at least one of [`LABELS`], is dropped.
const FILLER: &[&str] = &[
    "music", "musical", "musik", "full", "the", "new", "original", "animated", "performance", "with", "con", "in",
    "remaster", "remastered", "high", "quality",
];

/// Words that make a part of a title a label of the upload rather than of the song.
const LABELS: &[&str] = &[
    "official", "officiel", "oficial", "ufficiale", "offizielles", "video", "vídeo", "videoclip", "videoclipe", "clip",
    "audio", "lyric", "lyrics", "letra", "visualizer", "visualiser", "mv", "hd", "hq", "4k", "8k", "uhd",
];

/// Channel names are often the artist's with one of these after it ("ArtistVEVO").
const CHANNEL_SUFFIXES: &[&str] = &["vevo", "topic", "official", "officiel", "music", "tv"];

/// Where a title says who it is by: "Artist - Song".
const BY: &[&str] = &[" - ", " – ", " — ", " -- "];

/// What comes before a trailing label: "Song | Official Video".
const TRAILING: &[&str] = &[" | ", " // ", " - ", " – ", " — ", " ~ "];

/// Between the names of a title's artists: "A & B - Song".
const NAMES: &[&str] = &[" & ", ", ", " x ", " feat. ", " ft. ", " featuring ", " feat ", " ft ", " with "];

/// Where a title credits other artists: "Song feat. B".
const FEATURING: &[&str] = &[" feat. ", " ft. ", " featuring ", " feat ", " ft "];

/// The song's title and artists, from an upload's title and the artists it
/// credits (for a video, its channel). Labels of the upload are dropped, and
/// "Artist - Song" becomes "Song" when the artist is the credited one; the
/// credited artists stay when they name them the same way, else the names
/// in the title replace them ("TaylorSwiftVEVO" becomes "Taylor Swift").
pub fn tidy(title: &str, artists: &[String]) -> (String, Vec<String>) {
    let mut tidied = drop_trailing(&drop_groups(title, is_label), is_label);
    let mut credited = artists.to_vec();
    if let Some((by, song)) = split_first(&tidied, BY)
        && let Some(names) = credit(by, artists)
    {
        let song = drop_trailing(unquote(song), is_label);
        if !song.is_empty() {
            tidied = song;
            credited = names;
        }
    }
    if tidied.is_empty() {
        return (title.trim().to_owned(), artists.to_vec());
    }
    (tidied, credited)
}

/// The same for two uploads of one song ("Song", "Song (Official Video)",
/// "Artist - Song [4K]", "Song (feat. B) - 2011 Remaster"), by the first
/// artist; `None` for a title with no letters or digits.
pub fn song_key(title: &str, artists: &[String]) -> Option<String> {
    let (title, artists) = tidy(title, artists);
    let versionless = |part: &str| is_label(part) || is_credit(part) || is_remaster(part);
    let title = drop_trailing(&drop_groups(&title, versionless), versionless);
    let title = match find_any(&title, FEATURING) {
        Some((at, _)) => &title[..at],
        None => &title,
    };
    let title = squash(title);
    let artist = artists.first().map(|a| artist_key(a)).unwrap_or_default();
    (!title.is_empty()).then(|| format!("{artist}\u{1f}{title}"))
}

/// Where a file saved as `<Artist>/<Album>/<Title> [<id>].<ext>` belongs
/// once its song is retitled from `old` to `new` (title, first artist):
/// named after the new title, and in the new artist's folder if it was in
/// the old artist's. `None` when it is there already.
pub fn retitled_path(path: &Path, id: &str, old: (&str, Option<&str>), new: (&str, Option<&str>)) -> Option<PathBuf> {
    let mut file = path.file_name()?.to_owned();
    if old.0 != new.0 {
        let ext = path.extension()?.to_str()?;
        file = file_name(new.0, id, ext).into();
    }
    let album_dir = path.parent()?;
    let mut dir = album_dir.to_owned();
    if let (Some(old_artist), Some(new_artist)) = (old.1, new.1)
        && old_artist != new_artist
    {
        let artist_dir = album_dir.parent()?;
        if artist_dir.file_name() == Some(OsStr::new(&name_part(old_artist))) {
            dir = artist_dir.parent()?.join(name_part(new_artist)).join(album_dir.file_name()?);
        }
    }
    let moved = dir.join(file);
    (moved != path).then_some(moved)
}

/// Moves a file to `to` (see [`retitled_path`]), then removes its old album
/// and artist folders if that left them empty.
pub fn relocate(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::rename(from, to)?;
    for dir in from.ancestors().skip(1).take(2) {
        if fs::remove_dir(dir).is_err() {
            break;
        }
    }
    Ok(())
}

/// Where a song goes in the music folder: `<Artist>/<Album>/<Title> [<id>].<ext>`,
/// as [`crate::DEFAULT_TEMPLATE`] has yt-dlp put downloads (a song with no
/// album is in `Singles`).
pub fn library_path(artist: &str, album: Option<&str>, title: &str, id: &str, ext: &str) -> PathBuf {
    let artist = if artist.trim().is_empty() { "Unknown Artist" } else { artist };
    let album = album.filter(|a| !a.trim().is_empty()).unwrap_or("Singles");
    PathBuf::from(name_part(artist)).join(name_part(album)).join(file_name(title, id, ext))
}

/// `<title> [<id>].<ext>`, as yt-dlp names downloads.
pub fn file_name(title: &str, id: &str, ext: &str) -> String {
    format!("{} [{id}].{ext}", name_part(title))
}

/// A title or name as yt-dlp puts it in a path with `--windows-filenames`:
/// characters Windows doesn't allow become full-width look-alikes.
fn name_part(s: &str) -> String {
    let mapped: String = s
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '/' => '\u{29F8}',
            '\\' => '\u{29F9}',
            '"' | '*' | ':' | '<' | '>' | '?' | '|' => char::from_u32(c as u32 + 0xFEE0).unwrap_or('_'),
            c => c,
        })
        .collect();
    let trimmed = mapped.trim().trim_end_matches('.').trim_end();
    if trimmed.is_empty() { "_".to_owned() } else { trimmed.to_owned() }
}

/// Whether a part of a title only labels the upload: "Official Video", "HD",
/// "Lyrics", "M/V".
fn is_label(part: &str) -> bool {
    let lower = part.to_lowercase().replace("m/v", "mv");
    let words = words(&lower);
    !words.is_empty()
        && words.iter().all(|w| FILLER.contains(w) || LABELS.contains(w))
        && words.iter().any(|w| LABELS.contains(w))
}

/// "feat. B", "with B", "prod. C".
fn is_credit(part: &str) -> bool {
    let lower = part.to_lowercase();
    words(&lower).first().is_some_and(|w| matches!(*w, "feat" | "ft" | "featuring" | "with" | "prod"))
}

/// "Remastered 2011", "2009 Remaster".
fn is_remaster(part: &str) -> bool {
    let lower = part.to_lowercase();
    words(&lower).iter().any(|w| w.starts_with("remaster"))
}

fn words(s: &str) -> Vec<&str> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect()
}

/// `title` without the bracketed parts `drop` picks.
fn drop_groups(title: &str, drop: impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(title.len());
    let mut rest = title;
    while let Some(start) = rest.find(['(', '[', '【']) {
        let open = rest[start..].chars().next().unwrap_or('(');
        let close = match open {
            '(' => ')',
            '[' => ']',
            _ => '】',
        };
        let inner_start = start + open.len_utf8();
        let Some(len) = rest[inner_start..].find(close) else { break };
        let inner = &rest[inner_start..inner_start + len];
        let end = inner_start + len + close.len_utf8();
        out.push_str(&rest[..start]);
        if !drop(inner) {
            out.push_str(&rest[start..end]);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `title` without trailing parts `drop` picks: "Song | Official Video".
fn drop_trailing(title: &str, drop: impl Fn(&str) -> bool) -> String {
    let mut title = title.trim();
    while let Some((head, tail)) = split_last(title, TRAILING) {
        if head.trim().is_empty() || !drop(tail) {
            break;
        }
        title = head.trim();
    }
    title.to_owned()
}

/// The first of `seps` in `s`: its position and length.
fn find_any(s: &str, seps: &[&str]) -> Option<(usize, usize)> {
    let lower = s.to_ascii_lowercase();
    seps.iter().filter_map(|sep| lower.find(sep).map(|at| (at, sep.len()))).min()
}

fn split_first<'a>(s: &'a str, seps: &[&str]) -> Option<(&'a str, &'a str)> {
    let (at, len) = find_any(s, seps)?;
    Some((&s[..at], &s[at + len..]))
}

fn split_last<'a>(s: &'a str, seps: &[&str]) -> Option<(&'a str, &'a str)> {
    let (at, len) = seps.iter().filter_map(|sep| s.rfind(sep).map(|at| (at, sep.len()))).max()?;
    Some((&s[..at], &s[at + len..]))
}

/// A song title in quotes: `"Song"`, “Song”.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\''), ('«', '»'), ('「', '」')] {
        if let Some(inner) = s.strip_prefix(open).and_then(|s| s.strip_suffix(close))
            && !inner.trim().is_empty()
        {
            return inner.trim();
        }
    }
    s
}

/// The artists of a title's "By - Song" when `by` names the credited artist:
/// the credited list when it is the same name, else the names in `by`.
fn credit(by: &str, artists: &[String]) -> Option<Vec<String>> {
    let by = by.trim();
    if squash(by).is_empty() {
        return None;
    }
    let Some(first) = artists.first() else { return Some(vec![by.to_owned()]) };
    if squash(by) == squash(first) {
        return Some(artists.to_vec());
    }
    let channel = channel_names(first);
    if channel.contains(&squash(by)) {
        return Some(vec![by.to_owned()]);
    }
    let names = split_names(by);
    (names.len() > 1 && names.iter().any(|n| channel.contains(&squash(n)))).then_some(names)
}

/// The names an artist's channel may go by, squashed: "ArtistVEVO" is "artistvevo" and "artist".
fn channel_names(channel: &str) -> Vec<String> {
    let full = squash(channel);
    let mut names = vec![full.clone()];
    names.extend(CHANNEL_SUFFIXES.iter().filter_map(|s| full.strip_suffix(s)).filter(|s| !s.is_empty()).map(str::to_owned));
    names
}

/// An artist as [`song_key`] compares them: "TaylorSwiftVEVO" and "Taylor Swift" are one.
fn artist_key(artist: &str) -> String {
    let full = squash(artist);
    ["vevo", "topic", "official"]
        .iter()
        .find_map(|s| full.strip_suffix(s).filter(|s| !s.is_empty()))
        .map_or_else(|| full.clone(), str::to_owned)
}

/// "A & B feat. C" -> ["A", "B", "C"].
fn split_names(by: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = by;
    while let Some((head, tail)) = split_first(rest, NAMES) {
        names.push(head.trim().to_owned());
        rest = tail;
    }
    names.push(rest.trim().to_owned());
    names.retain(|n| !squash(n).is_empty());
    names
}

/// Lowercase letters and digits only.
fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn tidied(title: &str, artists: &[&str]) -> (String, Vec<String>) {
        tidy(title, &v(artists))
    }

    #[test]
    fn drops_upload_labels() {
        for (title, want) in [
            ("Anti-Hero (Official Music Video)", "Anti-Hero"),
            ("Anti-Hero [Official Lyric Video]", "Anti-Hero"),
            ("Anti-Hero (Official Audio) [HD]", "Anti-Hero"),
            ("Anti-Hero (Official 4K Video)", "Anti-Hero"),
            ("Anti-Hero (Lyrics)", "Anti-Hero"),
            ("Anti-Hero (Visualizer)", "Anti-Hero"),
            ("Anti-Hero 【MV】", "Anti-Hero"),
            ("Anti-Hero (M/V)", "Anti-Hero"),
            ("Anti-Hero (Video Oficial)", "Anti-Hero"),
            ("Anti-Hero | Official Video", "Anti-Hero"),
            ("Anti-Hero - Official Audio", "Anti-Hero"),
            ("Anti-Hero // Lyric Video", "Anti-Hero"),
            ("Anti-Hero (Remastered in 4K)", "Anti-Hero"),
            ("Anti-Hero (Official Video) (4K Remaster)", "Anti-Hero"),
            ("Anti-Hero (Official Animated Video)", "Anti-Hero"),
        ] {
            assert_eq!(tidied(title, &["Taylor Swift"]).0, want, "{title}");
        }
    }

    #[test]
    fn keeps_what_names_the_version() {
        for title in [
            "Song (Live)",
            "Song (Acoustic)",
            "Song (Radio Edit)",
            "Song (Remastered 2011)",
            "Song (2009 Remaster)",
            "Song (Live 1987)",
            "Song (Countdown, 1987)",
            "Song (feat. Other)",
            "Song (Official Remix)",
            "Song (Video Edit)",
            "Song - Live at Wembley",
            "Official",
            "(Official Video)",
            "Video Games",
            "Audio",
        ] {
            assert_eq!(tidied(title, &["Artist"]).0, title, "{title}");
        }
    }

    #[test]
    fn moves_the_artist_out_of_the_title() {
        // The channel is the artist's, under another name.
        assert_eq!(
            tidied("Taylor Swift - Anti-Hero (Official Music Video)", &["TaylorSwiftVEVO"]),
            ("Anti-Hero".into(), v(&["Taylor Swift"]))
        );
        assert_eq!(tidied("Daft Punk - Get Lucky", &["Daft Punk Official"]), ("Get Lucky".into(), v(&["Daft Punk"])));
        // The same name: the credited artists stay (YouTube Music's list).
        assert_eq!(tidied("Artist - Song", &["artist", "Guest"]), ("Song".into(), v(&["artist", "Guest"])));
        // Several artists, one of them the channel's.
        assert_eq!(
            tidied("Calvin Harris & Dua Lipa - One Kiss (Official Video)", &["CalvinHarrisVEVO"]),
            ("One Kiss".into(), v(&["Calvin Harris", "Dua Lipa"]))
        );
        assert_eq!(tidied("Artist - \"Song\" | Official Video", &["Artist"]), ("Song".into(), v(&["Artist"])));
        assert_eq!(tidied("Artist – Song", &[]), ("Song".into(), v(&["Artist"])));
    }

    #[test]
    fn leaves_other_dashes_alone() {
        // A channel that isn't the artist's: can't tell a title from an artist.
        assert_eq!(tidied("Artist - Song (Official Video)", &["Label Records"]), ("Artist - Song".into(), v(&["Label Records"])));
        // YouTube Music's own titles.
        assert_eq!(tidied("Song - Remix", &["Artist"]), ("Song - Remix".into(), v(&["Artist"])));
        assert_eq!(tidied("Four - Five", &["Artist"]), ("Four - Five".into(), v(&["Artist"])));
    }

    #[test]
    fn keys_uploads_of_one_song_alike() {
        let key = |title: &str, artists: &[&str]| song_key(title, &v(artists));
        let song = key("Anti-Hero", &["Taylor Swift"]);
        assert!(song.is_some());
        for (title, artists) in [
            ("Taylor Swift - Anti-Hero (Official Music Video)", &["TaylorSwiftVEVO"][..]),
            ("Anti-Hero (Official Video)", &["TaylorSwiftVEVO"]),
            ("ANTI-HERO", &["Taylor Swift", "Guest"]),
            ("Anti-Hero (feat. Somebody)", &["Taylor Swift"]),
            ("Anti-Hero ft. Somebody", &["Taylor Swift"]),
            ("Anti-Hero - 2022 Remaster", &["Taylor Swift"]),
            ("Anti-Hero (Remastered)", &["Taylor Swift - Topic"]),
        ] {
            assert_eq!(key(title, artists), song, "{title} by {artists:?}");
        }
        for (title, artists) in [
            ("Anti-Hero (Live)", &["Taylor Swift"][..]),
            ("Anti-Hero (Acoustic)", &["Taylor Swift"]),
            ("Anti-Hero", &["Someone Else"]),
            ("Anti Heroes", &["Taylor Swift"]),
        ] {
            assert_ne!(key(title, artists), song, "{title} by {artists:?}");
        }
        assert_eq!(key("!!!", &["A"]), None);
        // Other scripts are letters too.
        assert!(key("夜に駆ける", &["YOASOBI"]).is_some());
        assert_eq!(key("夜に駆ける", &["YOASOBI"]), key("夜に駆ける (Official Music Video)", &["YOASOBI"]));
    }

    #[test]
    fn retitles_paths() {
        let path = Path::new("/m/TaylorSwiftVEVO/Singles/Taylor Swift - Anti-Hero (Official Music Video) [b1kbLwvqugk].m4a");
        let old = ("Taylor Swift - Anti-Hero (Official Music Video)", Some("TaylorSwiftVEVO"));
        assert_eq!(
            retitled_path(path, "b1kbLwvqugk", old, ("Anti-Hero", Some("Taylor Swift"))),
            Some(PathBuf::from("/m/Taylor Swift/Singles/Anti-Hero [b1kbLwvqugk].m4a"))
        );
        // Only the title changed.
        assert_eq!(
            retitled_path(path, "b1kbLwvqugk", old, ("What? No: \"x\"/y.", Some("TaylorSwiftVEVO"))),
            Some(PathBuf::from("/m/TaylorSwiftVEVO/Singles/What？ No： ＂x＂⧸y [b1kbLwvqugk].m4a"))
        );
        // A folder named after someone else (the album artist) stays.
        let album = Path::new("/m/Various Artists/Hits/Song (Official Video) [b1kbLwvqugk].m4a");
        assert_eq!(
            retitled_path(album, "b1kbLwvqugk", ("Song (Official Video)", Some("A")), ("Song", Some("B"))),
            Some(PathBuf::from("/m/Various Artists/Hits/Song [b1kbLwvqugk].m4a"))
        );
        assert_eq!(retitled_path(album, "b1kbLwvqugk", ("Same", Some("A")), ("Same", Some("A"))), None);
    }

    #[test]
    fn places_songs_in_the_library() {
        assert_eq!(
            library_path("AC/DC", Some("Back in Black"), "Hells Bells", "abc", "m4a"),
            PathBuf::from("AC⧸DC/Back in Black/Hells Bells [abc].m4a")
        );
        assert_eq!(library_path("", None, "Song", "abc", "opus"), PathBuf::from("Unknown Artist/Singles/Song [abc].opus"));
    }

    #[test]
    fn relocates_files() {
        let root = std::env::temp_dir().join(format!("ytmdl-titles-{}", std::process::id()));
        let from = root.join("Old/Singles/x [abc].m4a");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, b"x").unwrap();
        let to = root.join("New/Singles/y [abc].m4a");
        relocate(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"x");
        assert!(!root.join("Old").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
