//! How loud a song is, for volume normalization. Songs from YouTube are
//! mastered at very different levels; playback evens them out with the
//! ReplayGain 2.0 tags written here (`REPLAYGAIN_TRACK_GAIN`/`_PEAK`, which
//! other players read too).
//!
//! The loudness is EBU R128 integrated loudness of the decoded audio (pure
//! Rust: symphonia decodes the AAC, ebur128 measures it). Only M4A/MP4 files
//! can be measured; others keep playing at their own level.

use std::fmt::Display;
use std::fs::File;
use std::path::Path;

use ebur128::{EbuR128, Mode};
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag};
use serde::{Deserialize, Serialize};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::error::{Error, Result};

/// ReplayGain 2.0's reference level, in LUFS.
pub const REFERENCE_LUFS: f64 = -18.0;

/// A song's ReplayGain.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Gain {
    /// dB to add to reach [`REFERENCE_LUFS`]; negative for loud songs.
    pub gain_db: f64,
    /// Largest sample, 1.0 being full scale.
    pub peak: f64,
}

/// Whether [`measure`] can decode files with this extension.
pub fn can_measure(ext: &str) -> bool {
    matches!(ext, "m4a" | "mp4")
}

/// Decodes the whole file and measures it. Blocking: about a second for a
/// song on a phone.
pub fn measure(path: &Path) -> Result<Gain> {
    let fail = |e: &dyn Display| Error::Invalid(format!("measuring {}: {e}", path.display()));
    let source = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, source, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| fail(&e))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| fail(&"no audio track"))?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| fail(&"no audio codec"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| fail(&e))?;

    let mut meter: Option<EbuR128> = None;
    let mut samples: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(e) => return Err(fail(&e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged packet costs its few milliseconds, not the measurement.
            Err(DecodeError::DecodeError(_)) => continue,
            Err(e) => return Err(fail(&e)),
        };
        let spec = decoded.spec();
        let channels = spec.channels().count() as u32;
        let meter = match &mut meter {
            Some(m) => m,
            None => meter.insert(EbuR128::new(channels, spec.rate(), Mode::I | Mode::SAMPLE_PEAK).map_err(|e| fail(&e))?),
        };
        decoded.copy_to_vec_interleaved(&mut samples);
        meter.add_frames_f32(&samples).map_err(|e| fail(&e))?;
    }
    let meter = meter.ok_or_else(|| fail(&"no audio"))?;
    let lufs = meter.loudness_global().map_err(|e| fail(&e))?;
    if !lufs.is_finite() {
        // Silence: nothing to even out.
        return Ok(Gain { gain_db: 0.0, peak: 0.0 });
    }
    let peak = (0..meter.channels()).filter_map(|c| meter.sample_peak(c).ok()).fold(0.0, f64::max);
    Ok(Gain { gain_db: REFERENCE_LUFS - lufs, peak })
}

/// Sets the ReplayGain tags in a tag.
pub fn set_tags(tag: &mut Tag, gain: Gain) {
    tag.insert_text(ItemKey::ReplayGainTrackGain, format!("{:.2} dB", gain.gain_db));
    tag.insert_text(ItemKey::ReplayGainTrackPeak, format!("{:.6}", gain.peak));
}

/// The ReplayGain a tag holds, if any.
pub fn from_tag(tag: &Tag) -> Option<Gain> {
    let gain = tag.get_string(ItemKey::ReplayGainTrackGain)?;
    let gain_db: f64 = gain.trim().trim_end_matches("dB").trim_end_matches("db").trim().parse().ok()?;
    let peak = tag.get_string(ItemKey::ReplayGainTrackPeak).and_then(|p| p.trim().parse().ok()).unwrap_or(1.0);
    gain_db.is_finite().then_some(Gain { gain_db, peak })
}

/// Stores the gain in the file, leaving its other tags alone.
pub fn write(path: &Path, gain: Gain) -> Result<()> {
    let err = |e: &dyn Display| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    };
    let mut file = lofty::read_from_path(path).map_err(|e| err(&e))?;
    if file.primary_tag().is_none() {
        let tag_type = file.primary_tag_type();
        file.insert_tag(Tag::new(tag_type));
    }
    set_tags(file.primary_tag_mut().expect("primary tag was just inserted"), gain);
    file.save_to_path(path, WriteOptions::default()).map_err(|e| err(&e))?;
    Ok(())
}

/// The gain stored in the file, if any.
pub fn read(path: &Path) -> Result<Option<Gain>> {
    let file = lofty::read_from_path(path).map_err(|e| Error::Tag {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    Ok(file.primary_tag().or_else(|| file.first_tag()).and_then(from_tag))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// A copy of `.deps/fixtures/tone.m4a` (`ytmdl-cli record-fixtures`: a
    /// 440 Hz sine at half scale), or `None` to skip the test without it.
    fn fixture(name: &str) -> Option<PathBuf> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deps/fixtures/tone.m4a");
        if !fixture.exists() {
            eprintln!("skipping: {} is missing", fixture.display());
            return None;
        }
        let path = std::env::temp_dir().join(format!("ytmdl-loudness-{}-{name}.m4a", std::process::id()));
        std::fs::copy(&fixture, &path).unwrap();
        Some(path)
    }

    #[test]
    fn measures_and_stores_gain() {
        let Some(path) = fixture("tone") else { return };
        let gain = measure(&path).unwrap();
        // ffmpeg's ebur128 filter reads the fixture as -21.8 LUFS with a
        // -14.5 dBFS sample peak.
        assert!((gain.gain_db - 3.8).abs() < 0.1, "{gain:?}");
        assert!((gain.peak - 0.188).abs() < 0.005, "{gain:?}");

        assert_eq!(read(&path).unwrap(), None);
        write(&path, gain).unwrap();
        let stored = read(&path).unwrap().unwrap();
        assert!((stored.gain_db - gain.gain_db).abs() < 0.01 && (stored.peak - gain.peak).abs() < 1e-5);
        std::fs::remove_file(&path).unwrap();
    }

    /// `YTMDL_MEASURE=<file> cargo test -p ytmdl-core measure_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn measure_file() {
        let path = std::env::var_os("YTMDL_MEASURE").expect("YTMDL_MEASURE");
        let gain = measure(Path::new(&path)).unwrap();
        println!("{:.2} LUFS, {gain:?}", REFERENCE_LUFS - gain.gain_db);
    }

    #[test]
    fn reads_gain_tags_as_written_elsewhere() {
        let mut tag = Tag::new(lofty::tag::TagType::Mp4Ilst);
        tag.insert_text(ItemKey::ReplayGainTrackGain, "-7.31 dB".into());
        assert_eq!(from_tag(&tag), Some(Gain { gain_db: -7.31, peak: 1.0 }));
        tag.insert_text(ItemKey::ReplayGainTrackGain, "+2.5 db".into());
        tag.insert_text(ItemKey::ReplayGainTrackPeak, "0.5".into());
        assert_eq!(from_tag(&tag), Some(Gain { gain_db: 2.5, peak: 0.5 }));
        tag.insert_text(ItemKey::ReplayGainTrackGain, "loud".into());
        assert_eq!(from_tag(&tag), None);
    }

    #[test]
    fn refuses_what_it_cannot_decode() {
        let path = std::env::temp_dir().join(format!("ytmdl-loudness-{}-junk.m4a", std::process::id()));
        std::fs::write(&path, b"not audio").unwrap();
        assert!(measure(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }
}
