//! Cover art resized once for the UI. Embedded covers are up to 1200 px, far
//! more than a list row needs; decoding dozens of those in the WebView would
//! stutter and use hundreds of MB.

use std::fs;
use std::io::BufWriter;
use std::path::PathBuf;

use image::codecs::jpeg::JpegEncoder;

/// List rows and grid tiles.
pub const ART_SMALL: u32 = 240;
/// Album and now-playing pages.
pub const ART_LARGE: u32 = 720;

pub(crate) struct ArtCache {
    dir: PathBuf,
}

impl ArtCache {
    pub fn new(dir: PathBuf) -> Self {
        ArtCache { dir }
    }

    pub fn name(key: &str, size: u32) -> String {
        format!("{key}-{size}.jpg")
    }

    /// Stores `data` (JPEG or PNG) at both sizes and returns its key, which is
    /// derived from the bytes so tracks of one album share one copy.
    pub fn store(&self, data: &[u8]) -> Option<String> {
        let key = format!("{:016x}", fnv1a(data));
        let paths = [ART_LARGE, ART_SMALL].map(|size| (size, self.dir.join(Self::name(&key, size))));
        if paths.iter().all(|(_, p)| p.exists()) {
            return Some(key);
        }
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let img = image::load_from_memory(data)?;
            fs::create_dir_all(&self.dir)?;
            for (size, path) in &paths {
                let scaled = if img.width() > *size || img.height() > *size { img.thumbnail(*size, *size) } else { img.clone() };
                let part = path.with_extension("part");
                let mut out = BufWriter::new(fs::File::create(&part)?);
                JpegEncoder::new_with_quality(&mut out, 85).encode_image(&scaled.to_rgb8())?;
                out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
                fs::rename(&part, path)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => Some(key),
            Err(e) => {
                tracing::warn!(target: "ytmdl", "cover art {key}: {e}");
                None
            }
        }
    }

    /// Resolves a name from [`ArtCache::name`]; anything else is refused.
    pub fn file(&self, name: &str) -> Option<PathBuf> {
        let stem = name.strip_suffix(".jpg")?;
        if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return None;
        }
        let path = self.dir.join(name);
        path.is_file().then_some(path)
    }
}

/// FNV-1a: stable across builds (unlike std's hasher), which keys stored in the
/// database need.
fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}
