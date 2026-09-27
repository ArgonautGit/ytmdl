use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{ImageFormat, Rgb, RgbImage};
use ytmdl_core::tag::{Cover, write_tags};
use ytmdl_core::{Downloaded, Entry, TrackMeta};

use super::*;

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ytmdl-library-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    RgbImage::from_pixel(width, height, Rgb([200, 40, 60])).write_to(&mut out, ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

fn meta(id: &str, title: &str, album: Option<&str>, artists: &[&str], track: Option<u32>) -> TrackMeta {
    TrackMeta {
        id: id.into(),
        url: format!("https://music.youtube.com/watch?v={id}"),
        title: title.into(),
        artists: artists.iter().map(|a| a.to_string()).collect(),
        album: album.map(Into::into),
        album_artists: Vec::new(),
        track_number: track,
        disc_number: None,
        year: Some(2008),
        duration_secs: Some(180.0),
        cover_urls: Vec::new(),
    }
}

/// Records a download of a placeholder (untaggable) file.
fn add(lib: &Library, dir: &Path, meta: TrackMeta) -> Track {
    let path = dir.join(format!("{} [{}].m4a", meta.title, meta.id));
    fs::write(&path, b"not really audio").unwrap();
    lib.add_download(&Downloaded { path, meta, tagged: false }).unwrap()
}

#[test]
fn migrates_once_and_reopens() {
    let tmp = TempDir::new("reopen");
    let db = tmp.0.join("library.db");
    let lib = Library::open(&db, &tmp.0.join("art")).unwrap();
    add(&lib, &tmp.0, meta("aaaaaaaaaaa", "One", None, &["A"], None));
    drop(lib);
    let lib = Library::open(&db, &tmp.0.join("art")).unwrap();
    assert_eq!(lib.tracks().unwrap().len(), 1);
    let version: u32 = lib.db().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version as usize, MIGRATIONS.len());
}

#[test]
fn groups_albums_and_artists() {
    let tmp = TempDir::new("groups");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    add(&lib, &tmp.0, meta("track000002", "Second", Some("Record"), &["Band"], Some(2)));
    add(&lib, &tmp.0, meta("track000001", "First", Some("Record"), &["Band", "Guest"], Some(1)));
    add(&lib, &tmp.0, meta("single00001", "Lonely", None, &["Guest"], None));

    let titles = |tracks: Vec<Track>| tracks.into_iter().map(|t| t.title).collect::<Vec<_>>();
    assert_eq!(titles(lib.tracks().unwrap()), ["Lonely", "First", "Second"]);

    let albums = lib.albums().unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!((albums[0].title.as_str(), albums[0].artist.as_str(), albums[0].tracks), ("Record", "Band", 2));
    assert_eq!(albums[0].duration_secs, 360.0);
    assert_eq!(titles(lib.album_tracks("Record", "Band").unwrap()), ["First", "Second"]);

    let artists = lib.artists().unwrap();
    let summary: Vec<_> = artists.iter().map(|a| (a.name.as_str(), a.tracks, a.albums)).collect();
    assert_eq!(summary, [("Band", 2, 1), ("Guest", 2, 1)]);
    assert_eq!(titles(lib.artist_tracks("Guest").unwrap()), ["First", "Lonely"]);

    assert!(lib.video_ids().unwrap().contains("single00001"));
}

#[test]
fn redownload_updates_in_place() {
    let tmp = TempDir::new("upsert");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let first = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "Old title", None, &["A"], None));
    let second = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "New title", None, &["A"], None));
    assert_eq!(first.id, second.id);
    assert_eq!(first.added_at, second.added_at);
    let tracks = lib.tracks().unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "New title");
}

#[test]
fn looks_up_tracks_in_order_and_keeps_settings() {
    let tmp = TempDir::new("lookup");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let a = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "A", None, &["X"], None));
    let b = add(&lib, &tmp.0, meta("bbbbbbbbbbb", "B", None, &["X"], None));
    let found: Vec<i64> = lib.tracks_by_id(&[b.id, 999, a.id]).unwrap().iter().map(|t| t.id).collect();
    assert_eq!(found, [b.id, a.id]);

    assert_eq!(lib.setting("player").unwrap(), None);
    lib.set_setting("player", "one").unwrap();
    lib.set_setting("player", "two").unwrap();
    assert_eq!(lib.setting("player").unwrap().as_deref(), Some("two"));
}

#[test]
fn deletes_track_and_file() {
    let tmp = TempDir::new("delete");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let track = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "Gone", None, &["A"], None));
    lib.delete_track(track.id).unwrap();
    assert!(!track.path.exists());
    assert!(lib.tracks().unwrap().is_empty());
}

#[test]
fn saves_download_queue() {
    let tmp = TempDir::new("jobs");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let entry = |id: &str| Entry {
        id: id.into(),
        url: format!("https://music.youtube.com/watch?v={id}"),
        title: format!("Song {id}"),
        artists: vec!["A".into()],
        album: Some("Record".into()),
        duration_secs: Some(100.0),
        thumbnail: None,
        kind: Some("song".into()),
        year: None,
        track_number: Some(3),
    };
    let a = lib.add_download_job(&entry("a")).unwrap();
    let b = lib.add_download_job(&entry("b")).unwrap();
    let c = lib.add_download_job(&entry("c")).unwrap();
    lib.set_download_state(a, &SavedState::Done { path: "/m/a.m4a".into(), tagged: true }).unwrap();
    lib.set_download_state(b, &SavedState::Failed("HTTP Error 403".into())).unwrap();

    let jobs = lib.download_jobs().unwrap();
    let states: Vec<_> = jobs.iter().map(|j| (j.id, j.state.clone())).collect();
    assert_eq!(
        states,
        [
            (a, SavedState::Done { path: "/m/a.m4a".into(), tagged: true }),
            (b, SavedState::Failed("HTTP Error 403".into())),
            (c, SavedState::Pending),
        ]
    );
    assert_eq!(jobs[2].entry, entry("c"));

    assert_eq!(lib.remove_finished_jobs().unwrap(), 2);
    assert_eq!(lib.download_jobs().unwrap().len(), 1);
}

#[test]
fn caches_resized_art() {
    let tmp = TempDir::new("art");
    let cache = ArtCache::new(tmp.0.join("art"));
    let key = cache.store(&jpeg(1000, 800)).unwrap();
    assert_eq!(cache.store(&jpeg(1000, 800)).as_deref(), Some(key.as_str()));
    for (size, dims) in [(ART_LARGE, (720, 576)), (ART_SMALL, (240, 192))] {
        let file = cache.file(&Library::art_name(&key, size)).unwrap();
        assert_eq!(image::image_dimensions(&file).unwrap(), dims);
    }
    assert!(cache.store(b"not an image").is_none());
    assert!(cache.file("../library.db").is_none());
    assert!(cache.file(&format!("../{key}-240.jpg")).is_none());
    assert!(cache.file("0123456789abcdef-240.jpg").is_none());
}

/// Needs `.deps/fixtures/tone.m4a` (`ytmdl-cli record-fixtures`); skipped without it.
#[test]
fn scan_indexes_tagged_files_and_forgets_deleted_ones() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deps/fixtures/tone.m4a");
    if !fixture.exists() {
        eprintln!("skipping: {} is missing", fixture.display());
        return;
    }
    let tmp = TempDir::new("scan");
    let music = tmp.0.join("Music");
    let album = music.join("Band/Record");
    fs::create_dir_all(&album).unwrap();
    let file = album.join("First [track000001].m4a");
    fs::copy(&fixture, &file).unwrap();
    fs::write(album.join("notes.txt"), "ignored").unwrap();
    let mut m = meta("track000001", "First", Some("Record"), &["Band", "Guest"], Some(1));
    m.album_artists = vec!["Band".into()];
    write_tags(&file, &m, Cover::sniff(jpeg(600, 600))).unwrap();

    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let report = lib.scan(std::slice::from_ref(&music)).unwrap();
    assert_eq!(report, ScanReport { added: 1, updated: 0, removed: 0 });
    let tracks = lib.tracks().unwrap();
    let t = &tracks[0];
    assert_eq!((t.video_id.as_str(), t.title.as_str()), ("track000001", "First"));
    assert_eq!(t.artists, ["Band", "Guest"]);
    assert_eq!((t.album.as_deref(), t.album_artist.as_str()), (Some("Record"), "Band"));
    assert_eq!((t.track_number, t.year), (Some(1), Some(2008)));
    assert_eq!(t.url, "https://music.youtube.com/watch?v=track000001");
    assert!(t.duration_secs.is_some_and(|d| d > 1.0));
    let art = t.art.clone().expect("cover indexed");
    assert!(lib.art_file(&Library::art_name(&art, ART_SMALL)).is_some());

    assert!(!lib.scan(std::slice::from_ref(&music)).unwrap().changed());

    // A folder that can't be listed is not taken as "everything was deleted".
    assert!(!lib.scan(&[tmp.0.join("missing")]).unwrap().changed());

    fs::remove_file(&file).unwrap();
    assert_eq!(lib.scan(&[music]).unwrap().removed, 1);
    assert!(lib.tracks().unwrap().is_empty());
}
