//! Song lyrics from LRCLIB (<https://lrclib.net>), a free database of lyrics,
//! many of them time-synced. YouTube Music shows lyrics too, but yt-dlp doesn't
//! extract them.
//!
//! Lyrics are stored in the file's lyrics tag: synced ones as LRC text
//! (`[01:23.45]A line`), which most music players read, plain ones as they are.

use serde::{Deserialize, Serialize};

use crate::{Downloader, encode_query};
use crate::error::Result;

const API: &str = "https://lrclib.net/api";
/// LRCLIB asks clients to say who they are.
const USER_AGENT: &str = concat!("ytmdl ", env!("CARGO_PKG_VERSION"), " (https://github.com/ArgonautGit/ytmdl)");
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// A search result this far from the song's length (seconds) is another recording.
const DURATION_SLACK: f64 = 3.0;

/// What to look lyrics up by.
#[derive(Debug, Clone, Copy)]
pub struct Song<'a> {
    pub title: &'a str,
    pub artists: &'a [String],
    pub album: Option<&'a str>,
    pub duration_secs: Option<f64>,
}

/// Lyrics as LRCLIB has them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Lyrics {
    /// LRC text.
    pub synced_lyrics: Option<String>,
    pub plain_lyrics: Option<String>,
    /// The song has no words.
    pub instrumental: bool,
    /// Seconds.
    pub duration: Option<f64>,
}

impl Lyrics {
    /// The text for the file's lyrics tag: the synced lyrics when there are
    /// some. `None` for an instrumental or an empty record.
    pub fn text(&self) -> Option<&str> {
        [&self.synced_lyrics, &self.plain_lyrics].into_iter().flatten().map(|t| t.trim()).find(|t| !t.is_empty())
    }
}

/// One line of lyrics.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// When it is sung (ms); `None` in plain lyrics.
    pub at_ms: Option<i64>,
    pub text: String,
}

/// Lines of stored lyrics. LRC comes back timed and in time order (a line
/// with several time tags appears once per tag); anything else as plain lines.
pub fn parse(text: &str) -> Vec<Line> {
    let mut timed = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(tag) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
            let Some(ms) = lrc_time(tag.0) else { break };
            times.push(ms);
            rest = tag.1;
        }
        let text = rest.trim().to_owned();
        timed.extend(times.into_iter().map(|ms| Line { at_ms: Some(ms), text: text.clone() }));
    }
    if !timed.is_empty() {
        timed.sort_by_key(|l| l.at_ms);
        return timed;
    }
    let lines: Vec<Line> = text.lines().map(|l| Line { at_ms: None, text: l.trim().to_owned() }).collect();
    // Keep blank lines between verses, not around the whole text.
    let start = lines.iter().position(|l| !l.text.is_empty()).unwrap_or(lines.len());
    let end = lines.iter().rposition(|l| !l.text.is_empty()).map_or(start, |i| i + 1);
    lines[start..end].to_vec()
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx` in ms; `None` for LRC metadata (`ar:…`).
fn lrc_time(tag: &str) -> Option<i64> {
    let (min, sec) = tag.split_once(':')?;
    let min: i64 = min.trim().parse().ok()?;
    let (whole, frac) = sec.split_once(['.', ':']).unwrap_or((sec, ""));
    let whole: i64 = whole.trim().parse().ok()?;
    if whole >= 60 || !frac.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 3 {
        return None;
    }
    let frac_ms = match frac.len() {
        0 => 0,
        n => frac.parse::<i64>().ok()? * 10_i64.pow(3 - n as u32),
    };
    Some((min * 60 + whole) * 1000 + frac_ms)
}

/// The line being sung at `position_ms` in timed lines (the last one started).
pub fn current_line(lines: &[Line], position_ms: i64) -> Option<usize> {
    lines.iter().rposition(|l| l.at_ms.is_some_and(|at| at <= position_ms))
}

fn query(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", encode_query(v))).collect::<Vec<_>>().join("&")
}

/// The exact-match lookup: title, main artist, and album and length when known.
fn get_url(song: &Song) -> Option<String> {
    let artist = song.artists.first()?;
    let duration = song.duration_secs.map(|d| format!("{}", d.round() as i64));
    let mut pairs = vec![("track_name", song.title), ("artist_name", artist.as_str())];
    if let Some(album) = song.album {
        pairs.push(("album_name", album));
    }
    if let Some(d) = &duration {
        pairs.push(("duration", d));
    }
    Some(format!("{API}/get?{}", query(&pairs)))
}

fn search_url(song: &Song) -> String {
    let q = match song.artists.first() {
        Some(artist) => format!("{artist} {}", song.title),
        None => song.title.to_owned(),
    };
    format!("{API}/search?{}", query(&[("q", &q)]))
}

/// The search result for this recording: one of about its length, with synced
/// lyrics if any has them.
fn pick(results: Vec<Lyrics>, duration_secs: Option<f64>) -> Option<Lyrics> {
    let fits = |l: &Lyrics| match (duration_secs, l.duration) {
        (Some(want), Some(have)) => (want - have).abs() <= DURATION_SLACK,
        _ => true,
    };
    let mut usable: Vec<Lyrics> =
        results.into_iter().filter(|l| fits(l) && (l.instrumental || l.text().is_some())).collect();
    if usable.is_empty() {
        return None;
    }
    let synced = usable.iter().position(|l| l.synced_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty()));
    Some(usable.swap_remove(synced.unwrap_or(0)))
}

impl Downloader {
    /// Looks the song up on LRCLIB: an exact match first, then a search.
    /// `None` when LRCLIB doesn't know it.
    pub async fn lyrics(&self, song: Song<'_>) -> Result<Option<Lyrics>> {
        let headers = vec![("User-Agent".to_owned(), USER_AGENT.to_owned())];
        if let Some(url) = get_url(&song)
            && let Some(body) = self.runtime().fetch_optional(url, MAX_RESPONSE_BYTES, headers.clone()).await?
        {
            let found: Lyrics = serde_json::from_slice(&body)?;
            if found.instrumental || found.text().is_some() {
                return Ok(Some(found));
            }
        }
        let Some(body) = self.runtime().fetch_optional(search_url(&song), MAX_RESPONSE_BYTES, headers).await? else {
            return Ok(None);
        };
        let results: Vec<Lyrics> = serde_json::from_slice(&body)?;
        Ok(pick(results, song.duration_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(at_ms: i64, text: &str) -> Line {
        Line { at_ms: Some(at_ms), text: text.into() }
    }

    /// Needs `.deps/fixtures/tone.m4a` (`ytmdl-cli record-fixtures`); skipped without it.
    #[test]
    fn stores_lyrics_in_files() {
        use crate::TrackMeta;
        use crate::tag::{read_lyrics, read_tags, write_lyrics, write_tags, write_tags_and_lyrics};

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deps/fixtures/tone.m4a");
        if !fixture.exists() {
            eprintln!("skipping: {} is missing", fixture.display());
            return;
        }
        let path = std::env::temp_dir().join(format!("ytmdl-lyrics-{}.m4a", std::process::id()));
        std::fs::copy(&fixture, &path).unwrap();
        let meta = TrackMeta {
            id: "abcdefghijk".into(),
            url: "https://music.youtube.com/watch?v=abcdefghijk".into(),
            title: "Song".into(),
            artists: vec!["Band".into()],
            album: None,
            album_artists: Vec::new(),
            track_number: None,
            disc_number: None,
            year: None,
            duration_secs: None,
            cover_urls: Vec::new(),
        };
        write_tags(&path, &meta, None).unwrap();
        assert_eq!(read_lyrics(&path).unwrap(), None);
        assert!(!read_tags(&path).unwrap().has_lyrics);

        let lrc = "[00:01.00]First line\n[00:02.50]Second line";
        write_tags_and_lyrics(&path, &meta, None, Some(lrc)).unwrap();
        assert_eq!(read_lyrics(&path).unwrap().as_deref(), Some(lrc));
        assert!(read_tags(&path).unwrap().has_lyrics);

        write_lyrics(&path, "Plain words").unwrap();
        assert_eq!(read_lyrics(&path).unwrap().as_deref(), Some("Plain words"));
        assert_eq!(read_tags(&path).unwrap().title.as_deref(), Some("Song"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn parses_lrc() {
        let lrc = "[ar:Band]\n[ti:Song]\n[00:31.48] Like the legend\n[00:35.4]All ends\n\n[01:02.345][00:10]Twice\n[00:48.82]";
        assert_eq!(
            parse(lrc),
            [
                timed(10_000, "Twice"),
                timed(31_480, "Like the legend"),
                timed(35_400, "All ends"),
                timed(48_820, ""),
                timed(62_345, "Twice"),
            ]
        );
    }

    #[test]
    fn parses_plain_lyrics() {
        let lines = parse("\nFirst line\n\nSecond verse\n\n");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["First line", "", "Second verse"]);
        assert!(lines.iter().all(|l| l.at_ms.is_none()));
        // A bracketed line that isn't a time stays text.
        assert_eq!(parse("[Chorus]\nLa la")[0], Line { at_ms: None, text: "[Chorus]".into() });
    }

    #[test]
    fn finds_the_current_line() {
        let lines = [timed(1_000, "a"), timed(5_000, "b"), timed(9_000, "c")];
        assert_eq!(current_line(&lines, 500), None);
        assert_eq!(current_line(&lines, 5_000), Some(1));
        assert_eq!(current_line(&lines, 60_000), Some(2));
        assert_eq!(current_line(&parse("plain"), 60_000), None);
    }

    #[test]
    fn prefers_synced_text() {
        let both = Lyrics { synced_lyrics: Some("[00:01.00]a".into()), plain_lyrics: Some("a".into()), ..Default::default() };
        assert_eq!(both.text(), Some("[00:01.00]a"));
        let plain = Lyrics { synced_lyrics: Some(" ".into()), plain_lyrics: Some("a\n".into()), ..Default::default() };
        assert_eq!(plain.text(), Some("a"));
        assert_eq!(Lyrics { instrumental: true, ..Default::default() }.text(), None);
    }

    #[test]
    fn builds_lookup_urls() {
        let artists = ["Daft Punk".to_string(), "Pharrell Williams".to_string()];
        let song = Song { title: "Get Lucky", artists: &artists, album: Some("R.A.M."), duration_secs: Some(368.6) };
        assert_eq!(
            get_url(&song).unwrap(),
            "https://lrclib.net/api/get?track_name=Get+Lucky&artist_name=Daft+Punk&album_name=R.A.M.&duration=369"
        );
        assert_eq!(search_url(&song), "https://lrclib.net/api/search?q=Daft+Punk+Get+Lucky");
        assert_eq!(get_url(&Song { artists: &[], ..song }), None);
    }

    #[test]
    fn picks_a_search_result_of_the_right_length() {
        let results: Vec<Lyrics> = serde_json::from_value(serde_json::json!([
            {"id": 1, "duration": 248.0, "syncedLyrics": "[00:01.00]radio edit", "plainLyrics": "radio edit"},
            {"id": 2, "duration": 367.0, "syncedLyrics": null, "plainLyrics": "album plain"},
            {"id": 3, "duration": 370.0, "syncedLyrics": "[00:01.00]album", "plainLyrics": "album"},
            {"id": 4, "duration": 369.0, "instrumental": false}
        ]))
        .unwrap();
        assert_eq!(pick(results.clone(), Some(369.0)).unwrap().text(), Some("[00:01.00]album"));
        assert_eq!(pick(results.clone(), Some(120.0)), None);
        assert_eq!(pick(results, None).unwrap().text(), Some("[00:01.00]radio edit"));
    }
}
