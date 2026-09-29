//! Fingerprint encoded audio samples and timing/configuration, excluding chapter
//! tracks and movable chunk offsets. Reads audio in bounded chunks.
use super::*;
#[derive(Clone, Copy)]
struct Atom {
    kind: [u8; 4],
    data: u64,
    end: u64,
}
fn atoms(r: &mut (impl Read + Seek), start: u64, end: u64) -> io::Result<Vec<Atom>> {
    let mut at = start;
    let mut out = Vec::new();
    while at < end {
        if end - at < 8 {
            return Err(invalid("truncated MP4 atom"));
        }
        r.seek(SeekFrom::Start(at))?;
        let mut h = [0; 8];
        r.read_exact(&mut h)?;
        let size = u32::from_be_bytes(h[..4].try_into().unwrap());
        let (size, header) = if size == 1 {
            let mut b = [0; 8];
            r.read_exact(&mut b)?;
            (u64::from_be_bytes(b), 16)
        } else if size == 0 {
            (end - at, 8)
        } else {
            (u64::from(size), 8)
        };
        let next = at.checked_add(size).ok_or_else(|| invalid("MP4 atom overflow"))?;
        if size < header || next > end {
            return Err(invalid("invalid MP4 atom size"));
        }
        out.push(Atom { kind: h[4..].try_into().unwrap(), data: at + header, end: next });
        if out.len() > 100_000 {
            return Err(invalid("too many MP4 atoms"));
        }
        at = next;
    }
    Ok(out)
}
fn child(r: &mut (impl Read + Seek), parent: Atom, name: &[u8; 4]) -> io::Result<Atom> {
    atoms(r, parent.data, parent.end)?.into_iter().find(|a| &a.kind == name).ok_or_else(|| invalid(format!("missing audio box {}", String::from_utf8_lossy(name))))
}
fn bytes(r: &mut (impl Read + Seek), a: Atom) -> io::Result<Vec<u8>> {
    let len = usize::try_from(a.end - a.data).map_err(invalid)?;
    if len > 64 * 1024 * 1024 {
        return Err(invalid("audio sample table exceeds verification limit"));
    }
    let mut b = vec![0; len];
    r.seek(SeekFrom::Start(a.data))?;
    r.read_exact(&mut b)?;
    Ok(b)
}
fn u32_at(b: &[u8], at: usize) -> io::Result<u32> {
    Ok(u32::from_be_bytes(b.get(at..at + 4).ok_or_else(|| invalid("truncated audio table"))?.try_into().unwrap()))
}
fn table(b: &[u8], header: usize, count: usize, width: usize) -> io::Result<()> {
    if header.checked_add(count.checked_mul(width).ok_or_else(|| invalid("table overflow"))?) != Some(b.len()) {
        return Err(invalid("audio table size mismatch"));
    }
    Ok(())
}

pub fn audio_fingerprint(r: &mut (impl Read + Seek)) -> io::Result<blake3::Hash> {
    let length = r.seek(SeekFrom::End(0))?;
    let roots = atoms(r, 0, length)?;
    if roots.iter().any(|a| a.kind == *b"moof") {
        return Err(invalid("fragmented MP4 enrichment is not supported"));
    }
    let moov = *roots.iter().find(|a| a.kind == *b"moov").ok_or_else(|| invalid("missing moov"))?;
    let media = roots.iter().filter(|a| a.kind == *b"mdat").copied().collect::<Vec<_>>();
    let mut hash = blake3::Hasher::new();
    let mut tracks = 0u32;
    let mut buffer = vec![0; 1024 * 1024];
    for track in atoms(r, moov.data, moov.end)?.into_iter().filter(|a| a.kind == *b"trak") {
        let mdia = child(r, track, b"mdia")?;
        let handler = child(r, mdia, b"hdlr")?;
        let handler = bytes(r, handler)?;
        if handler.get(8..12) != Some(b"soun") {
            continue;
        }
        tracks += 1;
        hash.update(&tracks.to_be_bytes());
        let mdhd = child(r, mdia, b"mdhd")?;
        hash.update(&bytes(r, mdhd)?);
        let minf = child(r, mdia, b"minf")?;
        let stbl = child(r, minf, b"stbl")?;
        let boxes = atoms(r, stbl.data, stbl.end)?;
        let required = |name: &[u8; 4]| boxes.iter().find(|a| &a.kind == name).copied().ok_or_else(|| invalid("missing audio sample table"));
        for kind in [b"stsd", b"stts"] {
            hash.update(&bytes(r, required(kind)?)?);
        }
        if let Some(ctts) = boxes.iter().find(|a| a.kind == *b"ctts") {
            hash.update(&bytes(r, *ctts)?);
        }
        let sizes = bytes(r, required(b"stsz")?)?;
        let fixed = u32_at(&sizes, 4)?;
        let count = u32_at(&sizes, 8)? as usize;
        table(&sizes, 12, if fixed == 0 { count } else { 0 }, 4)?;
        let mappings = bytes(r, required(b"stsc")?)?;
        let entries = u32_at(&mappings, 4)? as usize;
        table(&mappings, 8, entries, 12)?;
        if entries == 0 || u32_at(&mappings, 8)? != 1 {
            return Err(invalid("invalid audio chunk map"));
        }
        let offsets = boxes.iter().find(|a| a.kind == *b"co64").or_else(|| boxes.iter().find(|a| a.kind == *b"stco")).copied().ok_or_else(|| invalid("missing audio offsets"))?;
        let width = if offsets.kind == *b"co64" { 8 } else { 4 };
        let offsets = bytes(r, offsets)?;
        let chunks = u32_at(&offsets, 4)? as usize;
        table(&offsets, 8, chunks, width)?;
        let (mut sample, mut mapping) = (0usize, 0usize);
        for chunk in 1..=chunks {
            if mapping + 1 < entries && u32_at(&mappings, 8 + (mapping + 1) * 12)? as usize <= chunk {
                mapping += 1;
            }
            let n = u32_at(&mappings, 12 + mapping * 12)? as usize;
            if n == 0 || sample.checked_add(n).is_none_or(|s| s > count) {
                return Err(invalid("invalid audio samples per chunk"));
            }
            let mut chunk_length = 0u64;
            for index in sample..sample + n {
                let size = if fixed > 0 { fixed } else { u32_at(&sizes, 12 + index * 4)? };
                hash.update(&size.to_be_bytes());
                chunk_length = chunk_length.checked_add(u64::from(size)).ok_or_else(|| invalid("chunk overflow"))?;
            }
            sample += n;
            let start = 8 + (chunk - 1) * width;
            let offset = if width == 8 { u64::from_be_bytes(offsets[start..start + 8].try_into().unwrap()) } else { u64::from(u32_at(&offsets, start)?) };
            let end = offset.checked_add(chunk_length).ok_or_else(|| invalid("audio chunk overflow"))?;
            if !media.iter().any(|a| offset >= a.data && end <= a.end) {
                return Err(invalid("audio chunk outside media data"));
            }
            r.seek(SeekFrom::Start(offset))?;
            let mut remaining = chunk_length;
            while remaining > 0 {
                let n = remaining.min(buffer.len() as u64) as usize;
                r.read_exact(&mut buffer[..n])?;
                hash.update(&buffer[..n]);
                remaining -= n as u64;
            }
        }
        if sample != count {
            return Err(invalid("audio sample count mismatch"));
        }
    }
    if tracks == 0 {
        return Err(invalid("M4B has no verifiable audio tracks"));
    }
    Ok(hash.finalize())
}
