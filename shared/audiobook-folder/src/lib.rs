//! Folder-based audiobooks and their transport archive.
//!
//! A device keeps ordinary MP3 files. The archive exists only for transfer
//! and server storage; its entry order is the playback order.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const MAX_TRACKS: usize = 1024;
pub const MAX_NFO_BYTES: u64 = 256 * 1024;
pub const MAX_CUE_BYTES: u64 = 256 * 1024;
pub const MAX_COVER_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Default)]
pub struct Mp3Facts {
    pub duration_ms: u64,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub track_title: Option<String>,
    pub description: Option<String>,
    pub narrator: Option<String>,
    pub publisher: Option<String>,
    pub release_date: Option<String>,
    pub asin: Option<String>,
    pub isbn: Option<String>,
    pub artwork: Option<Vec<u8>>,
}

fn set_missing(slot: &mut Option<String>, value: String) {
    let value = value.trim();
    if slot.is_none() && !value.is_empty() { *slot = Some(value.to_owned()); }
}

impl Mp3Facts {
    fn absorb(&mut self, revision: &symphonia::core::meta::MetadataRevision, artwork: bool) {
        use symphonia::core::meta::{StandardTagKey as Key, StandardVisualKey};
        for tag in revision.tags() {
            let value = tag.value.to_string();
            match tag.std_key {
                Some(Key::Album) => set_missing(&mut self.album, value),
                Some(Key::AlbumArtist) => set_missing(&mut self.album_artist, value),
                Some(Key::Artist) => set_missing(&mut self.artist, value),
                Some(Key::TrackTitle) => set_missing(&mut self.track_title, value),
                Some(Key::Description | Key::Comment) => set_missing(&mut self.description, value),
                Some(Key::Performer) => set_missing(&mut self.narrator, value),
                Some(Key::Label) => set_missing(&mut self.publisher, value),
                Some(Key::ReleaseDate | Key::Date) => set_missing(&mut self.release_date, value),
                Some(Key::IdentAsin) => set_missing(&mut self.asin, value),
                _ => {
                    let key = tag.key.to_ascii_lowercase();
                    if key.contains("narrator") || key == "read by" { set_missing(&mut self.narrator, value); }
                    else if key.contains("publisher") { set_missing(&mut self.publisher, value); }
                    else if key.contains("isbn") { set_missing(&mut self.isbn, value); }
                    else if key.contains("asin") { set_missing(&mut self.asin, value); }
                }
            }
        }
        if artwork && self.artwork.is_none() {
            if let Some(visual) = revision.visuals().iter().find(|visual| visual.usage == Some(StandardVisualKey::FrontCover)).or_else(|| revision.visuals().first()) {
                if visual.data.len() as u64 <= MAX_COVER_BYTES { self.artwork = Some(visual.data.to_vec()); }
            }
        }
    }
}

/// Reads MP3 packet timestamps without decoding samples. The scan uses the
/// same result for chapter boundaries and the book-wide playback clock.
pub fn duration_ms<R: symphonia::core::io::MediaSource + 'static>(reader: R) -> io::Result<u64> {
    Ok(inspect_mp3(reader, false)?.duration_ms)
}

pub fn inspect_mp3<R: symphonia::core::io::MediaSource + 'static>(reader: R, artwork: bool) -> io::Result<Mp3Facts> {
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let source = MediaSourceStream::new(Box::new(reader), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut probed = symphonia::default::get_probe()
        .format(&hint, source, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(io::Error::other)?;
    let mut facts = Mp3Facts::default();
    if let Some(metadata) = probed.metadata.get() {
        if let Some(revision) = metadata.current() { facts.absorb(revision, artwork); }
    }
    let mut format = probed.format;
    if let Some(revision) = format.metadata().current() { facts.absorb(revision, artwork); }
    let track = format.default_track().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "MP3 has no audio track"))?;
    let track_id = track.id;
    let time_base = track.codec_params.time_base.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "MP3 has no time base"))?;
    let mut end = 0u64;
    loop {
        match format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => end = end.max(packet.ts().saturating_add(packet.dur())),
            Ok(_) => {},
            Err(Error::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    let duration = time_base.calc_time(end);
    let millis = duration.seconds.saturating_mul(1000).saturating_add((duration.frac * 1000.0).round() as u64);
    if millis == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "MP3 has no audio duration"));
    }
    facts.duration_ms = millis;
    if let Some(revision) = format.metadata().current() { facts.absorb(revision, artwork); }
    Ok(facts)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub name: String,
    pub path: PathBuf,
}

/// A single metadata sidecar at the root of an audiobook folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nfo {
    pub name: String,
    pub path: PathBuf,
}

pub type Cue = Nfo;
pub type Cover = Nfo;

fn discover_sidecar(folder: &Path, accepts: impl Fn(&str) -> bool, maximum: u64) -> io::Result<Option<Nfo>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        let Ok(name) = entry.file_name().into_string() else { continue };
        if name.starts_with('.') || !accepts(&name) { continue; }
        if !valid_sidecar_name(&name) || entry.metadata()?.len() > maximum {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid or oversized audiobook sidecar"));
        }
        files.push(Nfo { name, path: entry.path() });
    }
    if files.len() > 1 { return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook has multiple matching sidecars")); }
    Ok(files.pop())
}

pub fn discover_nfo(folder: &Path) -> io::Result<Option<Nfo>> {
    discover_sidecar(folder, |name| name.to_ascii_lowercase().ends_with(".nfo"), MAX_NFO_BYTES)
}

pub fn discover_cue(folder: &Path) -> io::Result<Option<Cue>> {
    discover_sidecar(folder, |name| name.to_ascii_lowercase().ends_with(".cue"), MAX_CUE_BYTES)
}

pub fn discover_cover(folder: &Path) -> io::Result<Option<Cover>> {
    for stem in ["cover", "folder"] {
        for extension in ["jpg", "jpeg", "png", "webp"] {
            if let Some(cover) = discover_sidecar(folder, |name| name.eq_ignore_ascii_case(&format!("{stem}.{extension}")), MAX_COVER_BYTES)? {
                return Ok(Some(cover));
            }
        }
    }
    Ok(None)
}

/// A matching sibling belongs to this M4B. A generic cover is unambiguous
/// only when this is the directory's sole M4B file.
pub fn discover_m4b_cover(path: &Path) -> io::Result<Option<Cover>> {
    let folder = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "M4B has no parent directory"))?;
    let stem = path.file_stem().and_then(|stem| stem.to_str()).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "M4B filename is not UTF-8"))?;
    for extension in ["jpg", "jpeg", "png", "webp"] {
        if let Some(cover) = discover_sidecar(folder, |name| name.eq_ignore_ascii_case(&format!("{stem}.{extension}")), MAX_COVER_BYTES)? {
            return Ok(Some(cover));
        }
    }
    let m4b_count = fs::read_dir(folder)?.filter_map(Result::ok).filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()) && entry.path().extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("m4b"))).count();
    if m4b_count == 1 { discover_cover(folder) } else { Ok(None) }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchivedTrack {
    pub name: String,
    pub offset: u64,
    pub length: u64,
}

fn audio_file(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
}

fn disc_number(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let rest = ["disc", "disk", "cd"].iter().find_map(|prefix| lower.strip_prefix(prefix))?.trim();
    (!rest.is_empty() && rest.chars().all(|character| character.is_ascii_digit())).then(|| rest.parse().ok()).flatten()
}

pub fn is_disc_folder_name(name: &str) -> bool { disc_number(name).is_some() }

#[derive(Default)]
struct TrackNumbers {
    disc: Option<u32>,
    track: Option<u32>,
}

fn positive_number(value: &str) -> Option<u32> {
    let first = value.trim().split('/').next()?.trim();
    first.parse::<u32>().ok().filter(|number| *number > 0)
}

fn read_track_numbers(path: &Path) -> io::Result<TrackNumbers> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
    use symphonia::core::probe::Hint;

    fn absorb(numbers: &mut TrackNumbers, revision: &MetadataRevision) {
        for tag in revision.tags() {
            match tag.std_key {
                Some(StandardTagKey::DiscNumber) if numbers.disc.is_none() => numbers.disc = positive_number(&tag.value.to_string()),
                Some(StandardTagKey::TrackNumber) if numbers.track.is_none() => numbers.track = positive_number(&tag.value.to_string()),
                _ => {},
            }
        }
    }

    let source = MediaSourceStream::new(Box::new(File::open(path)?), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut probed = symphonia::default::get_probe()
        .format(&hint, source, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(io::Error::other)?;
    let mut numbers = TrackNumbers::default();
    if let Some(metadata) = probed.metadata.get() {
        if let Some(revision) = metadata.current() { absorb(&mut numbers, revision); }
    }
    if let Some(revision) = probed.format.metadata().current() { absorb(&mut numbers, revision); }
    Ok(numbers)
}

fn order_from_tags(tracks: &mut [Track], use_disc_tags: bool) {
    let Some(mut tagged) = tracks.iter().map(|track| read_track_numbers(&track.path).ok()).collect::<Option<Vec<_>>>() else { return };
    if tagged.iter().any(|numbers| numbers.track.is_none()) { return; }
    let disc_tags_complete = use_disc_tags && tagged.iter().all(|numbers| numbers.disc.is_some());
    let mut seen = HashSet::new();
    if tagged.iter().any(|numbers| !seen.insert((if disc_tags_complete { numbers.disc } else { None }, numbers.track))) { return; }
    let mut indexed = tracks.iter().cloned().zip(tagged.drain(..)).collect::<Vec<_>>();
    indexed.sort_by(|(left, left_numbers), (right, right_numbers)| {
        (if disc_tags_complete { left_numbers.disc } else { None }, left_numbers.track)
            .cmp(&(if disc_tags_complete { right_numbers.disc } else { None }, right_numbers.track))
            .then_with(|| natural_cmp(&left.name, &right.name))
    });
    for (destination, (track, _)) in tracks.iter_mut().zip(indexed) { *destination = track; }
}

fn order_from_cue(folder: &Path, tracks: &mut Vec<Track>) -> io::Result<()> {
    let Some(cue) = discover_cue(folder)? else { return Ok(()) };
    let bytes = fs::read(cue.path)?;
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.trim_start_matches('\u{feff}').to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned(),
    };
    let mut order = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((command, rest)) = line.split_once(char::is_whitespace) else { continue };
        if !command.eq_ignore_ascii_case("FILE") { continue; }
        let rest = rest.trim();
        let file = if let Some(quoted) = rest.strip_prefix('"') {
            quoted.split_once('"').map(|(name, _)| name)
        } else {
            rest.split_whitespace().next()
        };
        let Some(file) = file else { return Ok(()) };
        let file = file.replace('\\', "/");
        let exact = tracks.iter().enumerate().filter(|(_, track)| track.name.eq_ignore_ascii_case(&file)).map(|(index, _)| index).collect::<Vec<_>>();
        let matching = if exact.is_empty() {
            tracks.iter().enumerate().filter(|(_, track)| track.name.rsplit('/').next().is_some_and(|name| name.eq_ignore_ascii_case(&file))).map(|(index, _)| index).collect::<Vec<_>>()
        } else { exact };
        if matching.len() != 1 { return Ok(()) }
        if !order.contains(&matching[0]) { order.push(matching[0]); }
    }
    if order.len() == tracks.len() {
        let previous = tracks.clone();
        *tracks = order.into_iter().map(|index| previous[index].clone()).collect();
    }
    Ok(())
}

fn identity_order(left: &str, right: &str) -> std::cmp::Ordering {
    fn group(name: &str) -> (u8, u32, &str, &str) {
        match name.split_once('/') {
            Some((disc, file)) if disc_number(disc).is_some() => (1, disc_number(disc).unwrap(), disc, file),
            _ => (0, 0, "", name),
        }
    }
    let (left_group, left_number, left_disc, left_file) = group(left);
    let (right_group, right_number, right_disc, right_file) = group(right);
    left_group.cmp(&right_group).then_with(|| left_number.cmp(&right_number))
        .then_with(|| natural_cmp(left_disc, right_disc)).then_with(|| natural_cmp(left_file, right_file))
}

fn natural_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let mut a = left.chars().peekable();
    let mut b = right.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let digits_a: String = std::iter::from_fn(|| a.next_if(|character| character.is_ascii_digit())).collect();
                let digits_b: String = std::iter::from_fn(|| b.next_if(|character| character.is_ascii_digit())).collect();
                let order = digits_a.trim_start_matches('0').len().cmp(&digits_b.trim_start_matches('0').len()).then_with(|| digits_a.trim_start_matches('0').cmp(digits_b.trim_start_matches('0')));
                if !order.is_eq() {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                a.next();
                b.next();
                let order = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if !order.is_eq() {
                    return order;
                }
            }
            (None, None) => return left.cmp(right),
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
        }
    }
}

/// Finds the tracks belonging to this book folder. Only numbered disc folders
/// are folded into the book; other child folders remain separate scan targets.
pub fn discover(folder: &Path) -> io::Result<Vec<Track>> {
    let mut tracks = Vec::new();
    let mut discs = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_file() && audio_file(&path) && !entry.file_name().to_string_lossy().starts_with('.') {
            let name = entry.file_name().into_string().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "MP3 filename is not UTF-8"))?;
            tracks.push(Track { name, path });
        } else if kind.is_dir() {
            let Ok(name) = entry.file_name().into_string() else { continue };
            if let Some(number) = disc_number(&name) {
                discs.push((number, name, path));
            }
        }
    }
    tracks.sort_by(|left, right| natural_cmp(&left.name, &right.name));
    order_from_tags(&mut tracks, true);
    discs.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| natural_cmp(&left.1, &right.1)));
    for (_, disc_name, path) in discs {
        let mut disc_tracks = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file() && audio_file(&path) && !entry.file_name().to_string_lossy().starts_with('.') {
                let filename = entry.file_name().into_string().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "MP3 filename is not UTF-8"))?;
                disc_tracks.push(Track { name: format!("{disc_name}/{filename}"), path });
            }
        }
        disc_tracks.sort_by(|left, right| natural_cmp(&left.name, &right.name));
        order_from_tags(&mut disc_tracks, false);
        tracks.extend(disc_tracks);
    }
    if !tracks.is_empty() { order_from_cue(folder, &mut tracks)?; }
    if tracks.len() > MAX_TRACKS {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook has too many MP3 tracks"));
    }
    Ok(tracks)
}

/// A portable identity for the source files, independent of playback order,
/// ZIP headers, and the folder's location. The canonical filename order
/// preserves identities assigned before ID3 and CUE ordering was supported.
pub fn identity(tracks: &[Track]) -> io::Result<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bokheim-mp3-folder-v1\0");
    let mut canonical = tracks.iter().collect::<Vec<_>>();
    canonical.sort_by(|left, right| identity_order(&left.name, &right.name));
    for track in canonical {
        hasher.update(&(track.name.len() as u64).to_le_bytes());
        hasher.update(track.name.as_bytes());
        hasher.update(&fs::metadata(&track.path)?.len().to_le_bytes());
        let mut file = File::open(&track.path)?;
        io::copy(&mut file, &mut HashWriter(&mut hasher))?;
    }
    Ok(hasher.finalize())
}

struct HashWriter<'a>(&'a mut blake3::Hasher);
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

/// Writes an ordinary ZIP with uncompressed MP3 entries. Stored entries can
/// be served directly by byte range without decoding the archive on the server.
pub fn write_archive<W: Write + Seek>(writer: W, tracks: &[Track]) -> io::Result<W> {
    write_archive_with_sidecars(writer, tracks, None, None, None)
}

pub fn write_archive_with_nfo<W: Write + Seek>(writer: W, tracks: &[Track], nfo: Option<&Nfo>) -> io::Result<W> {
    write_archive_with_sidecars(writer, tracks, nfo, None, None)
}

pub fn write_archive_with_sidecars<W: Write + Seek>(writer: W, tracks: &[Track], nfo: Option<&Nfo>, cue: Option<&Cue>, cover: Option<&Cover>) -> io::Result<W> {
    if tracks.is_empty() || tracks.len() > MAX_TRACKS {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "audiobook has no MP3 tracks"));
    }
    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut names = HashSet::new();
    for track in tracks {
        if !valid_entry_name(&track.name) || !names.insert(track.name.to_lowercase()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid or duplicate audiobook track name"));
        }
        zip.start_file(&track.name, options).map_err(io::Error::other)?;
        io::copy(&mut File::open(&track.path)?, &mut zip)?;
    }
    for (sidecar, maximum, kind) in [(nfo, MAX_NFO_BYTES, SidecarKind::Nfo), (cue, MAX_CUE_BYTES, SidecarKind::Cue), (cover, MAX_COVER_BYTES, SidecarKind::Cover)] {
        if let Some(sidecar) = sidecar {
            if sidecar_kind(&sidecar.name) != Some(kind) || !names.insert(sidecar.name.to_lowercase()) || fs::metadata(&sidecar.path)?.len() > maximum {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid or oversized audiobook sidecar"));
            }
            zip.start_file(&sidecar.name, options).map_err(io::Error::other)?;
            let size = io::copy(&mut File::open(&sidecar.path)?.take(maximum + 1), &mut zip)?;
            if size > maximum { return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook sidecar changed while archiving")); }
        }
    }
    zip.finish().map_err(io::Error::other)
}

fn valid_sidecar_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\\') && !name.contains(':') && !name.chars().any(char::is_control)
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum SidecarKind { Nfo, Cue, Cover }

fn sidecar_kind(name: &str) -> Option<SidecarKind> {
    if !valid_sidecar_name(name) { return None; }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".nfo") { Some(SidecarKind::Nfo) }
    else if lower.ends_with(".cue") { Some(SidecarKind::Cue) }
    else if ["cover.jpg", "cover.jpeg", "cover.png", "cover.webp", "folder.jpg", "folder.jpeg", "folder.png", "folder.webp"].contains(&lower.as_str()) { Some(SidecarKind::Cover) }
    else { None }
}

fn sidecar_limit(kind: SidecarKind) -> u64 {
    match kind { SidecarKind::Nfo => MAX_NFO_BYTES, SidecarKind::Cue => MAX_CUE_BYTES, SidecarKind::Cover => MAX_COVER_BYTES }
}

pub fn archived_cover<R: Read + Seek>(reader: R) -> io::Result<Option<Vec<u8>>> {
    let mut archive = ZipArchive::new(reader).map_err(io::Error::other)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(io::Error::other)?;
        if sidecar_kind(entry.name()) != Some(SidecarKind::Cover) { continue; }
        if entry.size() > MAX_COVER_BYTES { return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized audiobook cover")); }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        return Ok(Some(bytes));
    }
    Ok(None)
}

fn valid_entry_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\') && !part.contains(':') && !part.chars().any(char::is_control))
        && audio_file(Path::new(name))
}

/// Reads the byte positions needed to serve each MP3 as a separate resource.
pub fn archived_tracks<R: Read + Seek>(reader: R) -> io::Result<Vec<ArchivedTrack>> {
    let mut archive = ZipArchive::new(reader).map_err(io::Error::other)?;
    if archive.len() > MAX_TRACKS + 3 { return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook ZIP has too many entries")); }
    let mut tracks = Vec::with_capacity(archive.len());
    let mut names = HashSet::new();
    let mut sidecars = HashSet::new();
    let mut in_sidecars = false;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(io::Error::other)?;
        let name = entry.name().to_owned();
        if !names.insert(name.to_lowercase()) || entry.compression() != CompressionMethod::Stored {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid audiobook ZIP entry"));
        }
        if let Some(kind) = sidecar_kind(&name) {
            in_sidecars = true;
            if !sidecars.insert(kind) || entry.size() > sidecar_limit(kind) { return Err(io::Error::new(io::ErrorKind::InvalidData, "duplicate or oversized audiobook sidecar")); }
            continue;
        }
        if in_sidecars { return Err(io::Error::new(io::ErrorKind::InvalidData, "audio track follows audiobook sidecar")); }
        if !valid_entry_name(&name) { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid audiobook ZIP entry")); }
        let offset = entry.data_start().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "audiobook ZIP entry has no byte offset"))?;
        tracks.push(ArchivedTrack { name, offset, length: entry.size() });
    }
    if tracks.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook ZIP has no MP3 tracks"));
    }
    Ok(tracks)
}

/// Computes the same playback-order-independent identity from a received
/// archive before its contents are published as ordinary files.
pub fn archive_identity<R: Read + Seek>(reader: R) -> io::Result<blake3::Hash> {
    let mut archive = ZipArchive::new(reader).map_err(io::Error::other)?;
    if archive.len() > MAX_TRACKS + 3 { return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook ZIP has too many entries")); }
    let mut names = HashSet::new();
    let mut sidecars = HashSet::new();
    let mut tracks = Vec::new();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bokheim-mp3-folder-v1\0");
    let mut in_sidecars = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(io::Error::other)?;
        let name = entry.name().to_owned();
        if !names.insert(name.to_lowercase()) || entry.compression() != CompressionMethod::Stored {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid audiobook ZIP entry"));
        }
        if let Some(kind) = sidecar_kind(&name) {
            in_sidecars = true;
            if !sidecars.insert(kind) || entry.size() > sidecar_limit(kind) { return Err(io::Error::new(io::ErrorKind::InvalidData, "duplicate or oversized audiobook sidecar")); }
            io::copy(&mut entry, &mut io::sink())?;
            continue;
        }
        if in_sidecars { return Err(io::Error::new(io::ErrorKind::InvalidData, "audio track follows audiobook sidecar")); }
        if !valid_entry_name(&name) { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid audiobook ZIP entry")); }
        tracks.push((index, name, entry.size()));
    }
    if tracks.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook ZIP has no MP3 tracks"));
    }
    tracks.sort_by(|left, right| identity_order(&left.1, &right.1));
    for (index, name, size) in tracks {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&size.to_le_bytes());
        io::copy(&mut archive.by_index(index).map_err(io::Error::other)?, &mut HashWriter(&mut hasher))?;
    }
    Ok(hasher.finalize())
}

/// Extracts tracks into a caller-owned staging directory. The caller publishes
/// that directory only after this function returns successfully.
pub fn extract_archive<R: Read + Seek>(reader: R, destination: &Path) -> io::Result<Vec<String>> {
    let mut archive = ZipArchive::new(reader).map_err(io::Error::other)?;
    if archive.len() > MAX_TRACKS + 3 { return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook ZIP has too many entries")); }
    let mut names = Vec::with_capacity(archive.len());
    let mut seen = HashSet::new();
    let mut sidecars = HashSet::new();
    let mut in_sidecars = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(io::Error::other)?;
        let name = entry.name().to_owned();
        let sidecar = sidecar_kind(&name);
        if !(valid_entry_name(&name) || sidecar.is_some()) || !seen.insert(name.to_lowercase()) || entry.compression() != CompressionMethod::Stored || sidecar.is_some_and(|kind| entry.size() > sidecar_limit(kind) || !sidecars.insert(kind)) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid audiobook ZIP entry"));
        }
        if sidecar.is_some() { in_sidecars = true; }
        else if in_sidecars { return Err(io::Error::new(io::ErrorKind::InvalidData, "audio track follows audiobook sidecar")); }
        let output = destination.join(&name);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        io::copy(&mut entry, &mut File::create(output)?)?;
        names.push(name);
    }
    if !names.iter().any(|name| name.to_ascii_lowercase().ends_with(".mp3")) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook ZIP has no MP3 tracks"));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged_mp3(frames: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (key, value) in frames {
            body.extend_from_slice(key.as_bytes());
            body.extend_from_slice(&((value.len() + 1) as u32).to_be_bytes());
            body.extend_from_slice(&[0, 0, 0]);
            body.extend_from_slice(value.as_bytes());
        }
        let size = body.len() as u32;
        let mut bytes = vec![b'I', b'D', b'3', 3, 0, 0, ((size >> 21) & 0x7f) as u8, ((size >> 14) & 0x7f) as u8, ((size >> 7) & 0x7f) as u8, (size & 0x7f) as u8];
        bytes.extend(body);
        bytes.extend_from_slice(include_bytes!("../../../client/library-backend/crates/book-metadata/tests/fixtures/silence.mp3"));
        bytes
    }

    #[test]
    fn mp3_tags_supply_album_author_and_chapter_title() {
        let bytes = tagged_mp3(&[("TALB", "Tagged Book"), ("TPE2", "Tagged Author"), ("TIT2", "Chapter One")]);
        let facts = inspect_mp3(io::Cursor::new(bytes), false).unwrap();
        assert_eq!(facts.album.as_deref(), Some("Tagged Book"));
        assert_eq!(facts.album_artist.as_deref(), Some("Tagged Author"));
        assert_eq!(facts.track_title.as_deref(), Some("Chapter One"));
        assert!(facts.duration_ms > 0);
    }

    #[test]
    fn complete_id3_numbers_control_playback_order_without_changing_identity() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("A.mp3"), tagged_mp3(&[("TPOS", "2/2"), ("TRCK", "1/2")])).unwrap();
        fs::write(source.path().join("B.mp3"), tagged_mp3(&[("TPOS", "1/2"), ("TRCK", "2/2")])).unwrap();
        let tracks = discover(source.path()).unwrap();
        assert_eq!(tracks.iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["B.mp3", "A.mp3"]);
        let legacy = vec![tracks[1].clone(), tracks[0].clone()];
        let book_identity = identity(&tracks).unwrap();
        assert_eq!(book_identity, identity(&legacy).unwrap());
        let archive = write_archive(io::Cursor::new(Vec::new()), &tracks).unwrap().into_inner();
        assert_eq!(archive_identity(io::Cursor::new(&archive)).unwrap(), book_identity);
        assert_eq!(archived_tracks(io::Cursor::new(&archive)).unwrap().iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["B.mp3", "A.mp3"]);
    }

    #[test]
    fn incomplete_id3_numbers_leave_filename_order_intact() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("A.mp3"), tagged_mp3(&[("TRCK", "2")])).unwrap();
        fs::write(source.path().join("B.mp3"), tagged_mp3(&[])).unwrap();
        assert_eq!(discover(source.path()).unwrap().iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["A.mp3", "B.mp3"]);
    }

    #[test]
    fn cue_file_order_overrides_conflicting_id3_numbers() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("A.mp3"), tagged_mp3(&[("TRCK", "2")])).unwrap();
        fs::write(source.path().join("B.mp3"), tagged_mp3(&[("TRCK", "1")])).unwrap();
        fs::write(source.path().join("book.cue"), b"FILE \"A.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nFILE \"B.mp3\" MP3\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n").unwrap();
        let tracks = discover(source.path()).unwrap();
        assert_eq!(tracks.iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["A.mp3", "B.mp3"]);
        let archive = write_archive_with_sidecars(io::Cursor::new(Vec::new()), &tracks, None, discover_cue(source.path()).unwrap().as_ref(), None).unwrap().into_inner();
        assert_eq!(archive_identity(io::Cursor::new(&archive)).unwrap(), identity(&tracks).unwrap());
    }

    #[test]
    fn cue_full_paths_disambiguate_matching_disc_filenames() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("Disc 1")).unwrap();
        fs::create_dir(source.path().join("Disc 2")).unwrap();
        fs::write(source.path().join("Disc 1/01.mp3"), tagged_mp3(&[])).unwrap();
        fs::write(source.path().join("Disc 2/01.mp3"), tagged_mp3(&[])).unwrap();
        fs::write(source.path().join("book.cue"), b"FILE \"Disc 2/01.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nFILE \"Disc 1/01.mp3\" MP3\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n").unwrap();
        assert_eq!(discover(source.path()).unwrap().iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["Disc 2/01.mp3", "Disc 1/01.mp3"]);
    }

    #[test]
    fn mp3_front_cover_art_is_available_for_thumbnail_fallback() {
        let mut picture = vec![0];
        picture.extend_from_slice(b"image/png\0");
        picture.push(3);
        picture.push(0);
        picture.extend_from_slice(b"picture bytes");
        let mut frame = Vec::new();
        frame.extend_from_slice(b"APIC");
        frame.extend_from_slice(&(picture.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&picture);
        let size = frame.len() as u32;
        let mut bytes = vec![b'I', b'D', b'3', 3, 0, 0, ((size >> 21) & 0x7f) as u8, ((size >> 14) & 0x7f) as u8, ((size >> 7) & 0x7f) as u8, (size & 0x7f) as u8];
        bytes.extend(frame);
        bytes.extend_from_slice(include_bytes!("../../../client/library-backend/crates/book-metadata/tests/fixtures/silence.mp3"));
        let facts = inspect_mp3(io::Cursor::new(bytes), true).unwrap();
        assert_eq!(facts.artwork.as_deref(), Some(b"picture bytes".as_slice()));
    }

    #[test]
    fn folder_round_trip_keeps_disc_order_and_audio_bytes() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("Disc 2")).unwrap();
        fs::create_dir(source.path().join("Disc 1")).unwrap();
        fs::write(source.path().join("Disc 2/10 End.mp3"), b"last").unwrap();
        fs::write(source.path().join("Disc 1/2 Middle.mp3"), b"middle").unwrap();
        fs::write(source.path().join("Disc 1/1 Start.mp3"), b"first").unwrap();
        fs::write(source.path().join("book.nfo"), b"Title: A Book\n").unwrap();
        fs::write(source.path().join("book.cue"), b"FILE \"Disc 1/1 Start.mp3\" MP3\n").unwrap();
        fs::write(source.path().join("cover.jpg"), b"image").unwrap();
        let tracks = discover(source.path()).unwrap();
        assert_eq!(tracks.iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["Disc 1/1 Start.mp3", "Disc 1/2 Middle.mp3", "Disc 2/10 End.mp3"]);
        let original_identity = identity(&tracks).unwrap();
        let nfo = discover_nfo(source.path()).unwrap();
        let cue = discover_cue(source.path()).unwrap();
        let cover = discover_cover(source.path()).unwrap();
        let bytes = write_archive_with_sidecars(io::Cursor::new(Vec::new()), &tracks, nfo.as_ref(), cue.as_ref(), cover.as_ref()).unwrap().into_inner();
        assert_eq!(write_archive_with_sidecars(io::Cursor::new(Vec::new()), &tracks, nfo.as_ref(), cue.as_ref(), cover.as_ref()).unwrap().into_inner(), bytes);
        assert_eq!(archive_identity(io::Cursor::new(&bytes)).unwrap(), original_identity);
        let index = archived_tracks(io::Cursor::new(&bytes)).unwrap();
        assert_eq!(index.len(), 3);
        for (entry, expected) in index.iter().zip([b"first".as_slice(), b"middle".as_slice(), b"last".as_slice()]) {
            assert_eq!(&bytes[entry.offset as usize..][..entry.length as usize], expected);
        }
        let destination = tempfile::tempdir().unwrap();
        extract_archive(io::Cursor::new(&bytes), destination.path()).unwrap();
        assert_eq!(identity(&discover(destination.path()).unwrap()).unwrap(), original_identity);
        assert_eq!(fs::read(destination.path().join("book.nfo")).unwrap(), b"Title: A Book\n");
        assert_eq!(fs::read(destination.path().join("book.cue")).unwrap(), b"FILE \"Disc 1/1 Start.mp3\" MP3\n");
        assert_eq!(fs::read(destination.path().join("cover.jpg")).unwrap(), b"image");
    }

    #[test]
    fn rejects_archive_entries_outside_the_album() {
        assert!(!valid_entry_name("C:/escape.mp3"));
        let mut zip = ZipWriter::new(io::Cursor::new(Vec::new()));
        zip.start_file("../escape.mp3", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"x").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let output = tempfile::tempdir().unwrap();
        assert!(extract_archive(io::Cursor::new(bytes), output.path()).is_err());
        assert!(!output.path().parent().unwrap().join("escape.mp3").exists());
    }

    #[test]
    fn m4b_cover_requires_a_matching_name_or_an_unambiguous_folder() {
        let folder = tempfile::tempdir().unwrap();
        let first = folder.path().join("First.m4b");
        fs::write(&first, b"audio").unwrap();
        fs::write(folder.path().join("cover.jpg"), b"generic").unwrap();
        assert_eq!(discover_m4b_cover(&first).unwrap().unwrap().name, "cover.jpg");
        fs::write(folder.path().join("Second.m4b"), b"audio").unwrap();
        assert!(discover_m4b_cover(&first).unwrap().is_none());
        fs::write(folder.path().join("First.png"), b"specific").unwrap();
        assert_eq!(discover_m4b_cover(&first).unwrap().unwrap().name, "First.png");
    }
}
