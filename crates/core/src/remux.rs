//! Fragmented MP4 (DASH) -> regular MP4, without ffmpeg.
//!
//! YouTube's audio-only formats are fragmented MP4 (`ftyp` brand `dash`, sample
//! data in `moof`/`mdat` pairs). yt-dlp only rewrites them when ffmpeg is present,
//! which it never is on Android, and some players and taggers mishandle them. This
//! rebuilds the file as `ftyp` + `moov` (with full sample tables) + one `mdat`,
//! copying the sample description (`stsd`, i.e. the codec config) byte for byte.

use std::path::Path;

use crate::{Error, Result};

/// Rewrites `path` in place if it is a single-track fragmented MP4. Returns whether
/// it did anything.
pub fn defragment_mp4(path: &Path) -> Result<bool> {
    let data = std::fs::read(path)?;
    let Some(out) = defragment(&data).map_err(|m| Error::Invalid(format!("{}: {m}", path.display())))? else {
        return Ok(false);
    };
    let tmp = path.with_extension("defrag.part");
    std::fs::write(&tmp, &out)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

type R<T> = std::result::Result<T, String>;

#[derive(Clone, Copy)]
struct BoxRef<'a> {
    kind: [u8; 4],
    /// Offset of the box header in the file.
    start: usize,
    /// Payload (after the header).
    body: &'a [u8],
    /// Whole box, header included.
    raw: &'a [u8],
}

fn boxes(data: &[u8], base: usize) -> R<Vec<BoxRef<'_>>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size32 = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let (size, header) = match size32 {
            0 => ((data.len() - pos) as u64, 8),
            1 => {
                let large = data.get(pos + 8..pos + 16).ok_or("truncated box header")?;
                (u64::from_be_bytes(large.try_into().unwrap()), 16)
            }
            n => (n, 8),
        };
        let end = pos.checked_add(size as usize).filter(|&e| e <= data.len() && size >= header as u64);
        let end = end.ok_or_else(|| format!("box {} overruns its parent", String::from_utf8_lossy(&kind)))?;
        out.push(BoxRef { kind, start: base + pos, body: &data[pos + header..end], raw: &data[pos..end] });
        pos = end;
    }
    Ok(out)
}

fn find<'a>(list: &[BoxRef<'a>], kind: &[u8; 4]) -> Option<BoxRef<'a>> {
    list.iter().find(|b| &b.kind == kind).copied()
}

fn be32(b: &[u8], at: usize) -> R<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated field".into())
}

fn be64(b: &[u8], at: usize) -> R<u64> {
    b.get(at..at + 8).map(|s| u64::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated field".into())
}

struct Sample {
    offset: usize,
    size: u32,
    duration: u32,
}

/// Samples of one `trun` run, which are contiguous in the file.
struct Run {
    samples: Vec<Sample>,
}

fn defragment(data: &[u8]) -> R<Option<Vec<u8>>> {
    let top = boxes(data, 0)?;
    if find(&top, b"moof").is_none() {
        return Ok(None);
    }
    let moov = find(&top, b"moov").ok_or("no moov")?;
    let moov_children = boxes(moov.body, 0)?;
    let traks: Vec<_> = moov_children.iter().filter(|b| &b.kind == b"trak").collect();
    if traks.len() != 1 {
        return Err(format!("expected one track, found {}", traks.len()));
    }
    let track_id = {
        let tkhd = find(&boxes(traks[0].body, 0)?, b"tkhd").ok_or("no tkhd")?;
        be32(tkhd.body, if tkhd.body[0] == 1 { 20 } else { 12 })?
    };
    // Defaults from mvex/trex.
    let (mut def_duration, mut def_size) = (0u32, 0u32);
    if let Some(mvex) = find(&moov_children, b"mvex") {
        for trex in boxes(mvex.body, 0)?.iter().filter(|b| &b.kind == b"trex") {
            if be32(trex.body, 4)? == track_id {
                def_duration = be32(trex.body, 12)?;
                def_size = be32(trex.body, 16)?;
            }
        }
    }

    let mut runs = Vec::new();
    for moof in top.iter().filter(|b| &b.kind == b"moof") {
        let moof_children = boxes(moof.body, moof.start + (moof.raw.len() - moof.body.len()))?;
        let mut next_base: Option<usize> = None;
        for traf in moof_children.iter().filter(|b| &b.kind == b"traf") {
            let traf_children = boxes(traf.body, 0)?;
            let tfhd = find(&traf_children, b"tfhd").ok_or("no tfhd")?;
            let flags = be32(tfhd.body, 0)? & 0xff_ffff;
            if be32(tfhd.body, 4)? != track_id {
                continue;
            }
            let mut at = 8;
            let mut base = None;
            if flags & 0x01 != 0 {
                base = Some(be64(tfhd.body, at)? as usize);
                at += 8;
            }
            if flags & 0x02 != 0 {
                at += 4;
            }
            let (mut duration, mut size) = (def_duration, def_size);
            if flags & 0x08 != 0 {
                duration = be32(tfhd.body, at)?;
                at += 4;
            }
            if flags & 0x10 != 0 {
                size = be32(tfhd.body, at)?;
            }
            // Without an explicit base, data is relative to the moof (the only layout
            // YouTube and other DASH packagers produce for a single track).
            let mut data_pos = base.or(next_base).unwrap_or(moof.start);

            for trun in traf_children.iter().filter(|b| &b.kind == b"trun") {
                let body = trun.body;
                let tflags = be32(body, 0)? & 0xff_ffff;
                let count = be32(body, 4)? as usize;
                let mut at = 8;
                if tflags & 0x01 != 0 {
                    let offset = be32(body, at)? as i32;
                    data_pos = (base.unwrap_or(moof.start) as i64 + offset as i64) as usize;
                    at += 4;
                }
                if tflags & 0x04 != 0 {
                    at += 4;
                }
                let mut samples = Vec::with_capacity(count);
                for _ in 0..count {
                    let mut d = duration;
                    let mut s = size;
                    if tflags & 0x100 != 0 {
                        d = be32(body, at)?;
                        at += 4;
                    }
                    if tflags & 0x200 != 0 {
                        s = be32(body, at)?;
                        at += 4;
                    }
                    if tflags & 0x400 != 0 {
                        at += 4;
                    }
                    if tflags & 0x800 != 0 {
                        at += 4;
                    }
                    if data_pos + s as usize > data.len() {
                        return Err("sample data past end of file".into());
                    }
                    samples.push(Sample { offset: data_pos, size: s, duration: d });
                    data_pos += s as usize;
                }
                runs.push(Run { samples });
            }
            next_base = Some(data_pos);
        }
    }
    let total_samples: usize = runs.iter().map(|r| r.samples.len()).sum();
    if total_samples == 0 {
        return Err("no samples in fragments".into());
    }
    let media_duration: u64 = runs.iter().flat_map(|r| &r.samples).map(|s| s.duration as u64).sum();
    let mdat_len: u64 = runs.iter().flat_map(|r| &r.samples).map(|s| s.size as u64).sum();
    if mdat_len > u32::MAX as u64 - 1024 {
        return Err("too large to defragment".into());
    }

    let timescale = |b: BoxRef| be32(b.body, if b.body.first() == Some(&1) { 20 } else { 12 });
    let movie_timescale = timescale(find(&moov_children, b"mvhd").ok_or("no mvhd")?)?;
    let mdia = find(&boxes(traks[0].body, 0)?, b"mdia").ok_or("no mdia")?;
    let media_timescale = timescale(find(&boxes(mdia.body, 0)?, b"mdhd").ok_or("no mdhd")?)?;
    if media_timescale == 0 {
        return Err("zero media timescale".into());
    }
    let movie_duration = media_duration * movie_timescale as u64 / media_timescale as u64;
    let ctx = Ctx { runs: &runs, media_duration, movie_duration };

    let mut ftyp = Vec::new();
    write_box(&mut ftyp, b"ftyp", |b| {
        b.extend_from_slice(b"M4A ");
        b.extend_from_slice(&0u32.to_be_bytes());
        for brand in [b"M4A ", b"mp42", b"isom"] {
            b.extend_from_slice(brand);
        }
    });

    // Chunk offsets depend on the moov size, which does not depend on their values.
    let mut moov_out = rebuild_moov(moov.body, &ctx, 0)?;
    let mdat_start = (ftyp.len() + moov_out.len() + 8) as u32;
    moov_out = rebuild_moov(moov.body, &ctx, mdat_start)?;

    let mut out = Vec::with_capacity(ftyp.len() + moov_out.len() + 8 + mdat_len as usize);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov_out);
    out.extend_from_slice(&((mdat_len + 8) as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for s in runs.iter().flat_map(|r| &r.samples) {
        out.extend_from_slice(&data[s.offset..s.offset + s.size as usize]);
    }
    Ok(Some(out))
}

struct Ctx<'a> {
    runs: &'a [Run],
    /// In the track's (mdhd) timescale.
    media_duration: u64,
    /// In the movie's (mvhd) timescale.
    movie_duration: u64,
}

fn write_box(out: &mut Vec<u8>, kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(kind);
    body(out);
    let size = (out.len() - start) as u32;
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

fn rebuild_moov(body: &[u8], ctx: &Ctx, mdat_start: u32) -> R<Vec<u8>> {
    let mut out = Vec::new();
    let children = rebuild_children(body, ctx, mdat_start)?;
    write_box(&mut out, b"moov", |b| b.extend_from_slice(&children));
    Ok(out)
}

/// Re-emits a container's children, patching durations and replacing the sample tables.
fn rebuild_children(body: &[u8], ctx: &Ctx, mdat_start: u32) -> R<Vec<u8>> {
    let children = boxes(body, 0)?;
    let mut out = Vec::new();
    for child in &children {
        match &child.kind {
            b"mvex" | b"stts" | b"stsc" | b"stsz" | b"stz2" | b"stco" | b"co64" | b"stss" | b"ctts" | b"sgpd"
            | b"sbgp" | b"subs" | b"saiz" | b"saio" => {}
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                let inner = rebuild_children(child.body, ctx, mdat_start)?;
                write_box(&mut out, &child.kind, |b| b.extend_from_slice(&inner));
            }
            b"mvhd" => out.extend_from_slice(&patch_duration(child, 16, 24, ctx.movie_duration)?),
            b"tkhd" => out.extend_from_slice(&patch_duration(child, 20, 28, ctx.movie_duration)?),
            b"mdhd" => out.extend_from_slice(&patch_duration(child, 16, 24, ctx.media_duration)?),
            _ => out.extend_from_slice(child.raw),
        }
        if &child.kind == b"stsd" {
            write_sample_tables(&mut out, ctx, mdat_start);
        }
    }
    Ok(out)
}

/// Copies a full box, replacing the duration at `v0`/`v1` payload offsets.
fn patch_duration(b: &BoxRef, v0: usize, v1: usize, duration: u64) -> R<Vec<u8>> {
    let header = b.raw.len() - b.body.len();
    let mut raw = b.raw.to_vec();
    if b.body.first() == Some(&1) {
        raw.get_mut(header + v1..header + v1 + 8).ok_or("short header box")?.copy_from_slice(&duration.to_be_bytes());
    } else {
        let d = u32::try_from(duration).unwrap_or(u32::MAX);
        raw.get_mut(header + v0..header + v0 + 4).ok_or("short header box")?.copy_from_slice(&d.to_be_bytes());
    }
    Ok(raw)
}

fn write_sample_tables(out: &mut Vec<u8>, ctx: &Ctx, mdat_start: u32) {
    let samples = || ctx.runs.iter().flat_map(|r| &r.samples);

    // stts: run-length (count, duration).
    let mut stts: Vec<(u32, u32)> = Vec::new();
    for s in samples() {
        match stts.last_mut() {
            Some((n, d)) if *d == s.duration => *n += 1,
            _ => stts.push((1, s.duration)),
        }
    }
    write_box(out, b"stts", |b| {
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&(stts.len() as u32).to_be_bytes());
        for (n, d) in &stts {
            b.extend_from_slice(&n.to_be_bytes());
            b.extend_from_slice(&d.to_be_bytes());
        }
    });

    // One chunk per trun run; stsc lists (first_chunk, samples_per_chunk, desc=1).
    let chunks: Vec<&Run> = ctx.runs.iter().filter(|r| !r.samples.is_empty()).collect();
    let mut stsc: Vec<(u32, u32)> = Vec::new();
    for (i, run) in chunks.iter().enumerate() {
        let n = run.samples.len() as u32;
        if stsc.last().is_none_or(|&(_, per)| per != n) {
            stsc.push((i as u32 + 1, n));
        }
    }
    write_box(out, b"stsc", |b| {
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&(stsc.len() as u32).to_be_bytes());
        for (first, per) in &stsc {
            b.extend_from_slice(&first.to_be_bytes());
            b.extend_from_slice(&per.to_be_bytes());
            b.extend_from_slice(&1u32.to_be_bytes());
        }
    });

    write_box(out, b"stsz", |b| {
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes()); // sizes follow individually
        b.extend_from_slice(&(samples().count() as u32).to_be_bytes());
        for s in samples() {
            b.extend_from_slice(&s.size.to_be_bytes());
        }
    });

    write_box(out, b"stco", |b| {
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&(chunks.len() as u32).to_be_bytes());
        let mut offset = mdat_start;
        for run in &chunks {
            b.extend_from_slice(&offset.to_be_bytes());
            offset += run.samples.iter().map(|s| s.size).sum::<u32>();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_box(&mut out, kind, |b| b.extend_from_slice(body));
        out
    }

    fn full(version_flags: u32, rest: &[u8]) -> Vec<u8> {
        let mut v = version_flags.to_be_bytes().to_vec();
        v.extend_from_slice(rest);
        v
    }

    fn u32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// A one-track fragmented file with two fragments of 2 and 3 samples.
    fn fragmented() -> (Vec<u8>, Vec<u8>) {
        let mvhd = bx(b"mvhd", &full(0, &u32s(&[0, 0, 1000, 0, 0x10000])));
        let tkhd = bx(b"tkhd", &full(3, &u32s(&[0, 0, 1, 0, 0, 0, 0])));
        let mdhd = bx(b"mdhd", &full(0, &u32s(&[0, 0, 44100, 0, 0])));
        let stsd = bx(b"stsd", &full(0, &[0, 0, 0, 1, 0xAA, 0xBB]));
        let empty = |k: &[u8; 4]| bx(k, &full(0, &u32s(&[0])));
        let stbl = bx(b"stbl", &[stsd.clone(), empty(b"stts"), empty(b"stsc"), bx(b"stsz", &full(0, &u32s(&[0, 0]))), empty(b"stco")].concat());
        let minf = bx(b"minf", &stbl);
        let mdia = bx(b"mdia", &[mdhd, minf].concat());
        let trak = bx(b"trak", &[tkhd, mdia].concat());
        let trex = bx(b"trex", &full(0, &u32s(&[1, 1, 1024, 0, 0])));
        let mvex = bx(b"mvex", &trex);
        let moov = bx(b"moov", &[mvhd, trak, mvex].concat());
        let ftyp = bx(b"ftyp", b"dash\0\0\0\0iso6mp41");

        let payload: Vec<u8> = (0u8..50).collect();
        let mut file = [ftyp, moov].concat();
        let mut expected = Vec::new();
        let mut cursor = 0usize;
        for sizes in [&[10u32, 12][..], &[8, 9, 11][..]] {
            // tfhd: default-base-is-moof; trun: data-offset + sizes.
            let tfhd = bx(b"tfhd", &full(0x020000, &u32s(&[1])));
            let trun_body = |off: u32| {
                let mut v = full(0x000201, &u32s(&[sizes.len() as u32, off]));
                v.extend(u32s(sizes));
                v
            };
            let traf_len = 8 + tfhd.len() + 8 + trun_body(0).len();
            let moof_len = 8 + 16 + traf_len;
            let data_offset = (moof_len + 8) as u32;
            let traf = bx(b"traf", &[tfhd, bx(b"trun", &trun_body(data_offset))].concat());
            let moof = bx(b"moof", &[bx(b"mfhd", &full(0, &u32s(&[1]))), traf].concat());
            assert_eq!(moof.len(), moof_len);
            let n: usize = sizes.iter().map(|&s| s as usize).sum();
            let chunk = &payload[cursor..cursor + n];
            cursor += n;
            expected.extend_from_slice(chunk);
            file.extend(moof);
            file.extend(bx(b"mdat", chunk));
        }
        (file, expected)
    }

    #[test]
    fn rebuilds_sample_tables() {
        let (file, samples) = fragmented();
        let out = defragment(&file).unwrap().expect("fragmented input");
        let top = boxes(&out, 0).unwrap();
        let kinds: Vec<_> = top.iter().map(|b| String::from_utf8_lossy(&b.kind).into_owned()).collect();
        assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
        assert_eq!(&top[0].body[..4], b"M4A ");
        assert_eq!(find(&top, b"mdat").unwrap().body, &samples[..]);

        let moov = boxes(top[1].body, 0).unwrap();
        assert!(find(&moov, b"mvex").is_none());
        let mvhd = find(&moov, b"mvhd").unwrap();
        assert_eq!(be32(mvhd.body, 16).unwrap(), 5 * 1024 * 1000 / 44100);
        let trak = boxes(find(&moov, b"trak").unwrap().body, 0).unwrap();
        let mdia = boxes(find(&trak, b"mdia").unwrap().body, 0).unwrap();
        assert_eq!(be32(find(&mdia, b"mdhd").unwrap().body, 16).unwrap(), 5 * 1024);
        let minf = boxes(find(&mdia, b"minf").unwrap().body, 0).unwrap();
        let stbl = boxes(find(&minf, b"stbl").unwrap().body, 0).unwrap();
        assert_eq!(find(&stbl, b"stsd").unwrap().body, &[0, 0, 0, 0, 0, 0, 0, 1, 0xAA, 0xBB]);
        assert_eq!(find(&stbl, b"stts").unwrap().body, &u32s(&[0, 1, 5, 1024])[..]);
        assert_eq!(find(&stbl, b"stsc").unwrap().body, &u32s(&[0, 2, 1, 2, 1, 2, 3, 1])[..]);
        assert_eq!(find(&stbl, b"stsz").unwrap().body, &u32s(&[0, 0, 5, 10, 12, 8, 9, 11])[..]);
        let mdat_start = (top[2].start + 8) as u32;
        assert_eq!(find(&stbl, b"stco").unwrap().body, &u32s(&[0, 2, mdat_start, mdat_start + 22])[..]);
    }

    #[test]
    fn leaves_regular_files_alone() {
        let plain = [bx(b"ftyp", b"M4A \0\0\0\0"), bx(b"moov", &[]), bx(b"mdat", b"x")].concat();
        assert!(defragment(&plain).unwrap().is_none());
    }
}
