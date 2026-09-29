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
fn deletes_track_file_and_empty_folders() {
    let tmp = TempDir::new("delete");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let album = tmp.0.join("Music/Artist/Album");
    fs::create_dir_all(&album).unwrap();
    let track = add(&lib, &album, meta("aaaaaaaaaaa", "Gone", None, &["A"], None));
    let kept = add(&lib, &tmp.0, meta("bbbbbbbbbbb", "Kept", None, &["A"], None));
    assert_eq!(lib.delete_track(track.id).unwrap().map(|t| t.id), Some(track.id));
    assert!(!track.path.exists());
    assert!(!tmp.0.join("Music/Artist").exists());
    assert!(tmp.0.join("Music").exists());
    let left: Vec<i64> = lib.tracks().unwrap().iter().map(|t| t.id).collect();
    assert_eq!(left, [kept.id]);
    assert_eq!(lib.delete_track(track.id).unwrap(), None);
}

#[test]
fn playlists_keep_order_skip_duplicates_and_follow_deletes() {
    let tmp = TempDir::new("playlists");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let a = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "A", None, &["X"], None));
    let b = add(&lib, &tmp.0, meta("bbbbbbbbbbb", "B", None, &["X"], None));
    let c = add(&lib, &tmp.0, meta("ccccccccccc", "C", None, &["X"], None));

    let mix = lib.create_playlist("  Mix ").unwrap();
    let other = lib.create_playlist("Other").unwrap();
    assert_eq!(lib.add_to_playlist(mix, &[c.id, a.id]).unwrap(), 2);
    assert_eq!(lib.add_to_playlist(mix, &[a.id, b.id]).unwrap(), 1);
    lib.add_to_playlist(other, &[a.id]).unwrap();

    let titles = |id| lib.playlist_tracks(id).unwrap().into_iter().map(|e| e.track.title).collect::<Vec<_>>();
    assert_eq!(titles(mix), ["C", "A", "B"]);
    let names: Vec<String> = lib.playlists().unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["Other", "Mix"]);
    let p = lib.playlist(mix).unwrap().unwrap();
    assert_eq!((p.tracks, p.duration_secs), (3, 540.0));

    let entry = lib.playlist_tracks(mix).unwrap()[0].entry_id;
    lib.remove_from_playlist(entry).unwrap();
    assert_eq!(titles(mix), ["A", "B"]);

    lib.delete_track(a.id).unwrap();
    assert_eq!(titles(mix), ["B"]);
    assert!(titles(other).is_empty());

    lib.rename_playlist(mix, "Best").unwrap();
    assert_eq!(lib.playlist(mix).unwrap().unwrap().name, "Best");
    lib.delete_playlist(mix).unwrap();
    assert!(lib.playlist(mix).unwrap().is_none());
    assert_eq!(lib.tracks().unwrap().len(), 2);
}

#[test]
fn synced_playlists_follow_youtube() {
    let tmp = TempDir::new("synced");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let a = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "A", None, &["X"], None));
    let b = add(&lib, &tmp.0, meta("bbbbbbbbbbb", "B", None, &["X"], None));
    let url = "https://music.youtube.com/playlist?list=PL1";
    let id = lib.create_synced_playlist("Road trip", url).unwrap();
    assert_eq!(lib.create_synced_playlist("Again", url).unwrap(), id);
    assert_eq!(lib.synced_playlist(url).unwrap(), Some(id));

    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let titles = || lib.playlist_tracks(id).unwrap().into_iter().map(|e| e.track.title).collect::<Vec<_>>();
    // YouTube lists C (not downloaded yet), B, A.
    let missing = lib.sync_playlist(id, &ids(&["ccccccccccc", "bbbbbbbbbbb", "aaaaaaaaaaa"])).unwrap();
    assert_eq!(missing, ["ccccccccccc"]);
    assert_eq!(titles(), ["B", "A"]);
    let p = lib.playlist(id).unwrap().unwrap();
    assert!(p.is_synced() && p.synced_at.is_some());
    assert_eq!((p.tracks, p.wanted), (2, 3));

    // C's download finishes and takes its place.
    add(&lib, &tmp.0, meta("ccccccccccc", "C", None, &["X"], None));
    assert_eq!(titles(), ["C", "B", "A"]);
    let b_entry = lib.playlist_tracks(id).unwrap()[1].entry_id;

    // YouTube drops A, adds D and reorders; B keeps its entry.
    let missing = lib.sync_playlist(id, &ids(&["bbbbbbbbbbb", "ddddddddddd", "ccccccccccc"])).unwrap();
    assert_eq!(missing, ["ddddddddddd"]);
    assert_eq!(titles(), ["B", "C"]);
    assert_eq!(lib.playlist_tracks(id).unwrap()[0].entry_id, b_entry);
    assert!(lib.track(a.id).unwrap().is_some(), "songs leaving the playlist stay in the library");

    // A song deleted here isn't downloaded again by the next sync...
    lib.delete_track(b.id).unwrap();
    let missing = lib.sync_playlist(id, &ids(&["bbbbbbbbbbb", "ddddddddddd", "ccccccccccc"])).unwrap();
    assert_eq!(missing, ["ddddddddddd"]);
    assert_eq!(lib.playlist(id).unwrap().unwrap().wanted, 2);
    // ...unless it is downloaded again by hand.
    add(&lib, &tmp.0, meta("bbbbbbbbbbb", "B", None, &["X"], None));
    assert_eq!(titles(), ["B", "C"]);
    assert_eq!(lib.playlist(id).unwrap().unwrap().wanted, 3);

    lib.stop_syncing(id).unwrap();
    assert!(!lib.playlist(id).unwrap().unwrap().is_synced());
    assert_eq!(titles(), ["B", "C"]);
    assert_eq!(lib.synced_playlist(url).unwrap(), None);
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

/// Needs `.deps/fixtures/tone.m4a`, like the test above.
#[test]
fn scan_keeps_artists_with_commas() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deps/fixtures/tone.m4a");
    if !fixture.exists() {
        eprintln!("skipping: {} is missing", fixture.display());
        return;
    }
    let tmp = TempDir::new("commas");
    let music = tmp.0.join("Music");
    let album = music.join("Tyler, The Creator/Record");
    fs::create_dir_all(&album).unwrap();
    let file = album.join("First [track000001].m4a");
    fs::copy(&fixture, &file).unwrap();
    let m = meta("track000001", "First", Some("Record"), &["Tyler, The Creator", "Kali Uchis"], Some(1));
    write_tags(&file, &m, None).unwrap();
    // Tagged the way ytmdl did before it wrote ARTISTS.
    use lofty::prelude::*;
    let mut tagged = lofty::read_from_path(&file).unwrap();
    tagged.primary_tag_mut().unwrap().remove_key(lofty::tag::ItemKey::TrackArtists);
    tagged.save_to_path(&file, lofty::config::WriteOptions::default()).unwrap();

    // The index, filled from the download, has the right names.
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    lib.add_download(&Downloaded { path: file.clone(), meta: m, tagged: true }).unwrap();
    // A scan writes them into the file, and doesn't count that as a change.
    assert!(!lib.scan(std::slice::from_ref(&music)).unwrap().changed());
    assert_eq!(ytmdl_core::tag::read_tags(&file).unwrap().artists, ["Tyler, The Creator", "Kali Uchis"]);

    // An index rebuilt from the files keeps them.
    let fresh = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    assert_eq!(fresh.scan(&[music]).unwrap().added, 1);
    assert_eq!(fresh.tracks().unwrap()[0].artists, ["Tyler, The Creator", "Kali Uchis"]);
    let names: Vec<String> = fresh.artists().unwrap().into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["Kali Uchis", "Tyler, The Creator"]);
}

#[test]
fn imports_listens_and_counts_plays() {
    let tmp = TempDir::new("listens");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let a = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "Tune", Some("Record"), &["Band", "Guest"], Some(1)));
    let b = add(&lib, &tmp.0, meta("bbbbbbbbbbb", "Other", Some("Record"), &["Band"], Some(2)));
    let gone = add(&lib, &tmp.0, meta("ccccccccccc", "Gone", None, &["Solo"], None));
    let now = now();

    // Three plays of a, one of b, a skip of b (under 30 s), a bad line, and a
    // listen of a song deleted before the import.
    let log = tmp.0.join("listens.log");
    let line = |t: &Track, ms: i64| format!("{now}\t{}\t{ms}\n", t.id);
    let text = [line(&a, 180_000), line(&a, 45_000), line(&a, 30_000), line(&b, 90_000), line(&b, 5_000)].concat()
        + "garbage\n"
        + &line(&gone, 60_000);
    fs::write(&log, text).unwrap();
    lib.delete_track(gone.id).unwrap();
    assert_eq!(lib.import_listens(&log).unwrap(), 6);
    assert!(!log.exists());
    assert_eq!(lib.import_listens(&log).unwrap(), 0);

    // A log left halfway by an earlier import goes in before the new one.
    fs::write(tmp.0.join("listens.importing"), line(&b, 40_000)).unwrap();
    fs::write(&log, line(&b, 1_000)).unwrap();
    assert_eq!(lib.import_listens(&log).unwrap(), 2);

    let stats = lib.stats(Period::Week).unwrap();
    assert_eq!(stats.listened_ms, 180_000 + 45_000 + 30_000 + 90_000 + 5_000 + 60_000 + 40_000 + 1_000);
    assert_eq!(stats.plays, 3 + 2 + 1);
    assert_eq!((stats.songs, stats.artists), (2, 2));
    let top: Vec<_> = stats.top_tracks.iter().map(|t| (t.track.title.as_str(), t.plays)).collect();
    assert_eq!(top, [("Tune", 3), ("Other", 2)]);
    let artists: Vec<_> = stats.top_artists.iter().map(|a| (a.name.as_str(), a.plays)).collect();
    assert_eq!(artists, [("Band", 5), ("Guest", 3)]);
    assert_eq!(stats.top_albums.len(), 1);
    assert_eq!((stats.top_albums[0].title.as_str(), stats.top_albums[0].plays), ("Record", 5));

    // Today is the last of seven day buckets and holds everything.
    assert_eq!(stats.buckets.len(), 7);
    assert!(stats.buckets.iter().all(|b| b.day.is_some() && b.weekday.is_some()));
    assert_eq!(stats.buckets.last().unwrap().ms, stats.listened_ms);
    assert_eq!(lib.stats(Period::Month).unwrap().buckets.len(), 30);
    assert_eq!(lib.stats(Period::Year).unwrap().buckets.len(), 12);
    let all = lib.stats(Period::All).unwrap();
    assert_eq!((all.plays, all.buckets.len()), (6, 1));

    let counts = lib.play_counts().unwrap();
    assert_eq!(counts.tracks.get("aaaaaaaaaaa"), Some(&3));
    assert_eq!(counts.albums.get(&("Band".to_string(), "Record".to_string())), Some(&5));
    assert_eq!(counts.artists.get("Guest"), Some(&3));

    // Older listens fall out of the week but stay in all time.
    lib.db().execute("UPDATE listens SET started_at = started_at - 40 * 86400 WHERE ms = 180000", []).unwrap();
    assert_eq!(lib.stats(Period::Week).unwrap().plays, 5);
    assert_eq!(lib.stats(Period::All).unwrap().plays, 6);
}

#[test]
fn saves_sections() {
    let tmp = TempDir::new("sections");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let solo = lib.add_section("aaaaaaaaaaa", " Solo ", 90_000, 120_000).unwrap();
    let chorus = lib.add_section("aaaaaaaaaaa", "Chorus", 30_000, 45_000).unwrap();
    lib.add_section("bbbbbbbbbbb", "Intro", 0, 10_000).unwrap();
    assert!(lib.add_section("aaaaaaaaaaa", "Backwards", 50_000, 40_000).is_err());
    assert!(lib.add_section("aaaaaaaaaaa", "  ", 0, 1_000).is_err());

    let names = |lib: &Library| lib.sections("aaaaaaaaaaa").unwrap().into_iter().map(|s| s.name).collect::<Vec<_>>();
    assert_eq!(names(&lib), ["Chorus", "Solo"]);
    lib.rename_section(solo.id, "Guitar solo").unwrap();
    lib.delete_section(chorus.id).unwrap();
    assert_eq!(names(&lib), ["Guitar solo"]);
    assert_eq!(lib.sections("bbbbbbbbbbb").unwrap().len(), 1);
}

/// Needs `.deps/fixtures/tone.m4a`, like `scan_indexes_tagged_files_and_forgets_deleted_ones`.
#[test]
fn keeps_and_measures_replaygain() {
    use ytmdl_core::loudness::{self, Gain};
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deps/fixtures/tone.m4a");
    if !fixture.exists() {
        eprintln!("skipping: {} is missing", fixture.display());
        return;
    }
    let tmp = TempDir::new("gain");
    let music = tmp.0.join("Music");
    fs::create_dir_all(&music).unwrap();
    let tagged = music.join("Tagged [track000001].m4a");
    let untagged = music.join("Untagged [track000002].m4a");
    for (file, id) in [(&tagged, "track000001"), (&untagged, "track000002")] {
        fs::copy(&fixture, file).unwrap();
        write_tags(file, &meta(id, "Song", None, &["Band"], None), None).unwrap();
    }
    loudness::write(&tagged, Gain { gain_db: -6.5, peak: 0.9 }).unwrap();

    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    // A download reads the gain its file was tagged with.
    let track = lib.add_download(&Downloaded { path: tagged.clone(), meta: meta("track000001", "Song", None, &["Band"], None), tagged: true }).unwrap();
    assert_eq!(track.gain, Some(Gain { gain_db: -6.5, peak: 0.9 }));
    // A scan finds the other, without a gain.
    assert_eq!(lib.scan(std::slice::from_ref(&music)).unwrap().added, 1);
    let unmeasured = lib.unmeasured().unwrap();
    assert_eq!(unmeasured.len(), 1);
    assert_eq!((unmeasured[0].path.as_path(), unmeasured[0].gain), (untagged.as_path(), None));

    // Measured and tagged, it is done, and the scan doesn't count the new tag as a change.
    let gain = loudness::measure(&untagged).unwrap();
    loudness::write(&untagged, gain).unwrap();
    lib.set_gain(unmeasured[0].id, Some(gain), true).unwrap();
    assert!(lib.unmeasured().unwrap().is_empty());
    assert!(!lib.scan(std::slice::from_ref(&music)).unwrap().changed());
    assert_eq!(lib.track(unmeasured[0].id).unwrap().unwrap().gain, Some(gain));

    // One that can't be measured isn't tried again.
    let junk = add(&lib, &music, meta("track000003", "Junk", None, &["Band"], None));
    assert_eq!(lib.unmeasured().unwrap().len(), 1);
    lib.set_gain(junk.id, None, false).unwrap();
    assert!(lib.unmeasured().unwrap().is_empty());

    // An index rebuilt from the files has the gains.
    let fresh = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    fresh.scan(&[music]).unwrap();
    let gains: Vec<Option<Gain>> = fresh.tracks_by_id(&[1, 2]).unwrap().into_iter().map(|t| t.gain).collect();
    assert!(gains.iter().all(Option::is_some), "{gains:?}");
}

#[test]
fn finds_songs_downloaded_twice() {
    let tmp = TempDir::new("duplicates");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let song = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "Anti-Hero", Some("Midnights"), &["Taylor Swift"], Some(3)));
    let video = add(&lib, &tmp.0, meta("bbbbbbbbbbb", "Taylor Swift - Anti-Hero (Official Music Video)", None, &["TaylorSwiftVEVO"], None));
    add(&lib, &tmp.0, meta("ccccccccccc", "Anti-Hero (Live)", Some("Tour"), &["Taylor Swift"], None));
    add(&lib, &tmp.0, meta("ddddddddddd", "Lavender Haze", Some("Midnights"), &["Taylor Swift"], Some(1)));

    let found = lib.duplicates().unwrap();
    assert_eq!(found.len(), 1);
    let ids: Vec<i64> = found[0].tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids, [song.id, video.id]);
    assert_eq!(lib.song_keys().unwrap().get(&found[0].key).map(|t| t.id), Some(song.id));

    // Kept, until a third upload shows up.
    lib.keep_duplicates(&found[0]).unwrap();
    assert!(lib.duplicates().unwrap().is_empty());
    add(&lib, &tmp.0, meta("eeeeeeeeeee", "Anti-Hero [4K]", None, &["Taylor Swift"], None));
    assert_eq!(lib.duplicates().unwrap()[0].tracks.len(), 3);
}

#[test]
fn fills_home() {
    let tmp = TempDir::new("home");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let a = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "Tune", Some("Record"), &["Band"], Some(1)));
    let b = add(&lib, &tmp.0, meta("bbbbbbbbbbb", "Other", Some("Record"), &["Band"], Some(2)));
    let old = add(&lib, &tmp.0, meta("ccccccccccc", "Old Favourite", Some("Earlier"), &["Band"], Some(1)));
    add(&lib, &tmp.0, meta("ddddddddddd", "Never Played", Some("Latest"), &["Band"], Some(1)));
    let home = lib.home().unwrap();
    assert!(home.recent.is_empty() && home.top.is_empty() && home.forgotten.is_empty());
    assert_eq!(home.added.first().map(|a| a.title.as_str()), Some("Latest"));
    assert!(home.hour < 24);

    let now = now();
    let line = |t: &Track, ago: i64, ms: i64| format!("{}\t{}\t{ms}\n", now - ago, t.id);
    let log = tmp.0.join("listens.log");
    let text = [
        line(&a, 300, 180_000),
        line(&a, 200, 180_000),
        line(&b, 100, 180_000),
        // A skip doesn't make it the last played.
        line(&a, 50, 2_000),
        line(&old, 60 * 86400, 180_000),
        line(&old, 50 * 86400, 180_000),
        line(&old, 40 * 86400, 180_000),
    ]
    .concat();
    fs::write(&log, text).unwrap();
    lib.import_listens(&log).unwrap();

    let home = lib.home().unwrap();
    let titles = |tracks: &[Track]| tracks.iter().map(|t| t.title.clone()).collect::<Vec<_>>();
    assert_eq!(titles(&home.recent), ["Other", "Tune", "Old Favourite"]);
    assert_eq!(titles(&home.top), ["Tune", "Other"]);
    assert_eq!(titles(&home.forgotten), ["Old Favourite"]);
}

/// Needs `.deps/fixtures/tone.m4a`, like the scan tests.
#[test]
fn tidies_titles_in_place() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deps/fixtures/tone.m4a");
    if !fixture.exists() {
        eprintln!("skipping: {} is missing", fixture.display());
        return;
    }
    let tmp = TempDir::new("retitle");
    let music = tmp.0.join("Music");
    let singles = music.join("TaylorSwiftVEVO/Singles");
    fs::create_dir_all(&singles).unwrap();
    let file = singles.join("Taylor Swift - Anti-Hero (Official Music Video) [bbbbbbbbbbb].m4a");
    fs::copy(&fixture, &file).unwrap();
    let m = meta("bbbbbbbbbbb", "Taylor Swift - Anti-Hero (Official Music Video)", None, &["TaylorSwiftVEVO"], None);
    write_tags(&file, &m, None).unwrap();
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    lib.add_download(&Downloaded { path: file.clone(), meta: m, tagged: true }).unwrap();
    let tidy = add(&lib, &tmp.0, meta("aaaaaaaaaaa", "Lavender Haze", Some("Midnights"), &["Taylor Swift"], Some(1)));

    let untidy = lib.untidy().unwrap();
    assert_eq!(untidy.len(), 1);
    assert_eq!((untidy[0].title.as_str(), untidy[0].artists.as_slice()), ("Anti-Hero", &["Taylor Swift".to_owned()][..]));
    assert!(untidy.iter().all(|r| r.track.id != tidy.id));

    let track = lib.retitle(&untidy[0], true).unwrap();
    let moved = music.join("Taylor Swift/Singles/Anti-Hero [bbbbbbbbbbb].m4a");
    assert_eq!(track.path, moved);
    assert_eq!((track.title.as_str(), track.album_artist.as_str()), ("Anti-Hero", "Taylor Swift"));
    assert!(!music.join("TaylorSwiftVEVO").exists());
    let tags = ytmdl_core::tag::read_tags(&moved).unwrap();
    assert_eq!((tags.title.as_deref(), tags.artists.as_slice()), (Some("Anti-Hero"), &["Taylor Swift".to_owned()][..]));
    assert!(lib.untidy().unwrap().is_empty());
    // The index already knows the file as it is now.
    assert!(!lib.scan(std::slice::from_ref(&music)).unwrap().changed());
    assert_eq!(lib.artists().unwrap().iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Taylor Swift"]);
}

#[test]
fn caches_songs_played_without_downloading() {
    let tmp = TempDir::new("cache");
    let lib = Library::open_in_memory(&tmp.0.join("art")).unwrap();
    let cache = tmp.0.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let fetch = |id: &str, title: &str| {
        let path = cache.join(format!("{id}.m4a"));
        fs::write(&path, vec![0u8; 1000]).unwrap();
        lib.add_cached(&Downloaded { path, meta: meta(id, title, Some("Record"), &["Band"], Some(1)), tagged: false }).unwrap()
    };
    let a = fetch("aaaaaaaaaaa", "A");
    assert!(a.id < 0);
    // Cached songs aren't in the library, but the player finds them.
    assert!(lib.tracks().unwrap().is_empty() && lib.video_ids().unwrap().is_empty());
    assert_eq!(lib.cached("aaaaaaaaaaa").unwrap().map(|t| t.id), Some(a.id));
    assert_eq!(lib.tracks_by_id(&[a.id]).unwrap()[0].title, "A");

    let b = fetch("bbbbbbbbbbb", "B");
    let c = fetch("ccccccccccc", "C");
    lib.db().execute("UPDATE cached SET played_at = played_at - 100 WHERE id = ?1", [-b.id]).unwrap();
    assert_eq!(lib.cache_size().unwrap(), 3000);
    // The least recently played goes first, unless it's queued.
    assert_eq!(lib.trim_cache(2000, &HashSet::from([b.id])).unwrap(), 1);
    assert!(lib.cached("aaaaaaaaaaa").unwrap().is_none() && !a.path.exists());
    assert!(lib.cached("bbbbbbbbbbb").unwrap().is_some());
    // A file left by a fetch that stopped halfway.
    fs::write(cache.join("ddddddddddd.m4a.part"), b"x").unwrap();
    assert_eq!(lib.sweep_cache(&cache).unwrap(), 1);
    assert_eq!(lib.cache_size().unwrap(), 2000);

    // Listens count by video, so a kept song keeps its history.
    let log = tmp.0.join("listens.log");
    fs::write(&log, format!("{}\t{}\t60000\n", now(), c.id)).unwrap();
    lib.import_listens(&log).unwrap();
    assert_eq!(lib.stats(Period::All).unwrap().plays, 1);

    let music = tmp.0.join("Music");
    let kept = lib.keep_cached(c.id, &music).unwrap();
    assert!(kept.id > 0);
    assert_eq!(kept.path, music.join("Band/Record/C [ccccccccccc].m4a"));
    assert!(kept.path.exists() && c.path.exists());
    assert_eq!(lib.video_ids().unwrap(), HashSet::from(["ccccccccccc".to_owned()]));
    assert_eq!(lib.track_by_video("ccccccccccc").unwrap().map(|t| t.id), Some(kept.id));
    assert_eq!(lib.stats(Period::All).unwrap().top_tracks[0].track.id, kept.id);
}

fn ids(from: i64, to: i64) -> Vec<i64> {
    (from..=to).collect()
}

#[test]
fn shuffling_again_plays_unheard_songs_first() {
    let tracks = ids(1, 10);
    let mut p = Progress::shuffled(&tracks, None, 7);
    assert_eq!(p.split, 10);
    let mut sorted = p.order.clone();
    sorted.sort();
    assert_eq!(sorted, tracks);

    // Listen through four songs, leaving in the fifth.
    let first_four: Vec<i64> = p.order[..4].to_vec();
    p.note(p.order[4], 30_000, false);
    assert_eq!(p.index, 4);
    for id in &first_four {
        assert!(p.heard.contains(id));
    }
    assert_eq!(p.heard.len(), 4);

    // Shuffling again starts with the six not heard, then the four.
    let q = Progress::shuffled(&tracks, Some(&p), 99);
    assert_eq!(q.split, 6);
    assert!(q.order[..6].iter().all(|id| !first_four.contains(id)));
    assert!(q.order[6..].iter().all(|id| first_four.contains(id)));
    assert_eq!(q.heard.len(), 4);
}

#[test]
fn a_round_of_shuffling_ends_and_the_next_starts_clean() {
    let tracks = ids(1, 4);
    let mut p = Progress::shuffled(&tracks, None, 3);
    for i in 0..4 {
        p.note(p.order[i], 0, false);
    }
    // Reached the last song: three heard, the fourth playing.
    assert_eq!(p.heard.len(), 3);
    p.note(p.order[3], 0, true);
    assert!(p.done);
    assert_eq!(p.heard.len(), 4);
    assert_eq!(p.split, 0);
    // All heard: the next shuffle is a new round over everything.
    let q = Progress::shuffled(&tracks, Some(&p), 5);
    assert_eq!((q.split, q.heard.len()), (4, 0));
}

#[test]
fn the_queue_carries_on_into_the_next_round() {
    let tracks = ids(1, 6);
    let mut p = Progress::shuffled(&tracks, None, 11);
    p.note(p.order[1], 0, false);
    let mut q = Progress::shuffled(&tracks, Some(&p), 12);
    assert_eq!(q.split, 5);
    // Play through the five unheard and into the heard one.
    q.note(q.order[5], 0, false);
    assert_eq!(q.split, 0);
    let before: Vec<i64> = q.order[..5].to_vec();
    let mut heard = q.heard.clone();
    heard.sort();
    let mut expected = before;
    expected.sort();
    assert_eq!(heard, expected);
}

#[test]
fn resume_follows_the_playlist_as_it_is_now() {
    let tracks = ids(1, 8);
    let mut p = Progress::shuffled(&tracks, None, 21);
    let current = p.order[3];
    p.note(current, 42_000, false);
    assert!(p.resumable());

    // Same playlist: same queue, same place.
    let same = p.resume(&tracks, 1).unwrap();
    assert_eq!(same.order, p.order);
    assert_eq!((same.index, same.position_ms), (3, 42_000));

    // A song removed from the heard part and one added.
    let removed = p.order[1];
    let mut now: Vec<i64> = tracks.iter().copied().filter(|&t| t != removed).collect();
    now.push(100);
    let r = p.resume(&now, 2).unwrap();
    assert!(!r.order.contains(&removed));
    assert_eq!(r.order.len(), 8);
    assert_eq!(r.order[r.index], current);
    assert_eq!(r.position_ms, 42_000);
    let at = r.order.iter().position(|&t| t == 100).unwrap();
    assert!(at > r.index && at <= r.split, "new song joins the unheard songs to come: {r:?}");

    // The current song deleted: carries on with the next one from its start.
    let without: Vec<i64> = tracks.iter().copied().filter(|&t| t != current).collect();
    let r = p.resume(&without, 3).unwrap();
    assert_eq!(r.order[r.index], p.order[4]);
    assert_eq!(r.position_ms, 0);

    // Nothing left after it.
    let mut last = Progress::shuffled(&tracks, None, 21);
    last.note(last.order[7], 1_000, false);
    let only_earlier: Vec<i64> = last.order[..7].to_vec();
    assert!(last.resume(&only_earlier, 4).is_none());
}

#[test]
fn a_plain_playlist_resumes_in_its_own_order() {
    let tracks = ids(1, 5);
    let mut p = Progress::in_order(&tracks, 0, None);
    assert!(!p.resumable());
    p.note(3, 5_000, false);
    assert!(p.resumable());
    assert!(p.heard.is_empty());
    // A song added to the front and the order changed since.
    let now = vec![9, 5, 4, 3, 2, 1];
    let r = p.resume(&now, 1).unwrap();
    assert_eq!(r.order, now);
    assert_eq!((r.current(), r.position_ms), (Some(3), 5_000));
    // A song queued to play next isn't the playlist's; it changes nothing.
    p.note(77, 1_000, false);
    assert_eq!((p.current(), p.position_ms), (Some(3), 5_000));
    // Finishing the last song leaves nothing to resume.
    p.note(5, 0, true);
    assert!(!p.resumable());
}

#[test]
fn keeps_playlist_progress_until_the_playlist_goes() {
    let tmp = TempDir::new("progress");
    let lib = Library::open_in_memory(&tmp.0).unwrap();
    let id = lib.create_playlist("Run").unwrap();
    assert!(lib.playlist_progress(id).unwrap().is_none());
    let mut p = Progress::shuffled(&ids(1, 5), None, 8);
    p.note(p.order[2], 9_000, false);
    lib.set_playlist_progress(id, &p).unwrap();
    assert_eq!(lib.playlist_progress(id).unwrap(), Some(p.clone()));
    p.note(p.order[3], 0, false);
    lib.set_playlist_progress(id, &p).unwrap();
    assert_eq!(lib.playlist_progress(id).unwrap(), Some(p));
    // Not stored for a playlist that isn't there.
    lib.set_playlist_progress(id + 1, &Progress::default()).unwrap();
    assert!(lib.playlist_progress(id + 1).unwrap().is_none());
    lib.delete_playlist(id).unwrap();
    assert!(lib.playlist_progress(id).unwrap().is_none());
}
