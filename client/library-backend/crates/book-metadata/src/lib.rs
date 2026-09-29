//! Extracts typed book and audiobook metadata without application or database state.

use book_model::BookFormat;
use book_model::{BookMetadataError, BookRecord, Contributor, AUTHOR_MARC_RELATOR_CODE};
use std::collections::HashSet;
use std::io::{Read, Seek};
use std::path::Path;
use std::time::Duration;

mod identity;
mod nfo;
mod cue;

pub use identity::{edition_identity_evidence, EditionIdentityEvidence};

pub use book_model::ChapterOrigin;

pub type MetadataResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudiobookChapterMetadata {
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudiobookMetadata {
    pub title: String,
    pub authors: Vec<Contributor>,
    pub description: String,
    pub duration_ms: u64,
    pub chapters: Vec<AudiobookChapterMetadata>,
    #[serde(default)]
    pub tracks: Vec<book_model::AudiobookTrack>,
    pub chapter_origin: ChapterOrigin,
    #[serde(default)]
    pub recording: metadata_contract::audible::RecordingEvidence,
}

impl AudiobookMetadata {
    /// Original recording evidence used for lookup and enrichment invalidation.
    pub fn audible_lookup_request(&self) -> metadata_contract::audible::LookupRequest {
        metadata_contract::audible::LookupRequest {
            title: self.title.clone(),
            authors: self.authors.iter().map(|author| author.name().to_owned()).collect(),
            duration_ms: self.duration_ms,
            recording: self.recording.clone(),
            chapters: self.chapters.iter().map(|chapter| metadata_contract::audible::Chapter { title: chapter.title.clone(), start_ms: chapter.start_ms, end_ms: chapter.end_ms }).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M4bMetadata {
    pub title: String,
    pub authors: Vec<String>,
    pub description: String,
    pub duration: Duration,
    pub chapters: Vec<book_model::AudiobookChapter>,
    pub chapter_origin: ChapterOrigin,
    pub recording: metadata_contract::audible::RecordingEvidence,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct InspectedBook {
    #[serde(default)]
    pub pdf: Option<pdf_view_common::PdfReaderMetadata>,
    pub metadata: BookRecord,
    pub audiobook: Option<AudiobookMetadata>,
    /// The book's own navigation document, kept rather than discarded:
    /// the detail page draws contents for books that have never been
    /// downloaded, and re-reading the file for every visit is not an option
    /// when the file may be remote. Empty where the format carries none — an
    /// M4B's chapters are audiobook metadata, which the audiobook path files as
    /// navigation of its own.
    #[serde(default)]
    pub toc: Vec<book_model::BookTocEntry>,
}

pub use book_inspection::{inspect_epub, inspect_epub_reader, inspect_pdf, inspect_pdf_reader, InspectedEpub, InspectedPdf};

/// Reads all metadata relevant to indexing. Formats without useful embedded
/// metadata receive a title derived from the file name.
pub fn inspect_book(path: &Path, format: BookFormat) -> MetadataResult<InspectedBook> {
    inspect_book_reader(path, format, std::fs::File::open(path)?)
}

/// Inspects a seekable book using the same validation on every platform.
pub fn inspect_book_reader(source_name: &Path, format: BookFormat, reader: impl Read + Seek + Send + Sync + 'static) -> MetadataResult<InspectedBook> {
    inspect_book_reader_with_checksum(source_name, format, reader, None)
}

pub fn inspect_book_reader_with_checksum(source_name: &Path, format: BookFormat, reader: impl Read + Seek + Send + Sync + 'static, checksum: Option<content_address::ContentHash>) -> MetadataResult<InspectedBook> {
    let mut pdf = None;
    let mut pdf_toc = Vec::new();
    let reader = if format == BookFormat::Pdf {
        let (reader, extracted, outline) = pdf_reader_core::inspect_reader_metadata(reader)?;
        pdf = extracted.valid_for(&extracted.checksum).then_some(extracted);
        pdf_toc = outline;
        reader
    } else {
        reader
    };
    let mut reader = reader;
    let (mut metadata, audiobook, toc) = match format {
        BookFormat::Epub => {
            // The navigation document is parsed here whatever happens — the
            // inspection times it as `toc_ms` — so keeping it costs nothing
            // and saves opening the file again to draw the contents.
            let inspected = inspect_epub_reader(source_name, reader)?;
            (inspected.metadata, None, inspected.toc)
        }
        BookFormat::Mobi => (book_inspection::inspect_mobi_metadata_reader(source_name, reader)?, None, Vec::new()),
        BookFormat::Pdf => (inspect_pdf_reader(source_name, reader)?.metadata, None, pdf_toc),
        BookFormat::M4b => {
            let audiobook = audiobook_metadata_from(mp4ameta::Tag::read_from(&mut reader)?)?;
            let subtitle = audiobook.recording.subtitle.as_deref().filter(|value| !value.trim().is_empty()).map(str::to_owned);
            let metadata = BookRecord { title: audiobook.title.clone(), subtitle, contributors: audiobook.authors.clone(), description: audiobook.description.clone(), book: recording_book(&audiobook.recording)? };
            (metadata, Some(audiobook), Vec::new())
        }
        BookFormat::Mp3Folder => {
            let (audiobook, book) = inspect_mp3_archive(source_name, reader, checksum)?;
            let metadata = BookRecord { title: audiobook.title.clone(), subtitle: audiobook.recording.subtitle.clone(), contributors: audiobook.authors.clone(), description: audiobook.description.clone(), book };
            (metadata, Some(audiobook), Vec::new())
        }
    };
    book_inspection::normalize_with_filename(&mut metadata, &source_name.to_string_lossy())?;
    Ok(InspectedBook { metadata, audiobook, toc, pdf })
}

fn inspect_mp3_archive(source_name: &Path, mut reader: impl Read + Seek, checksum: Option<content_address::ContentHash>) -> MetadataResult<(AudiobookMetadata, book_model::BookMetadata)> {
    let checksum = match checksum {
        Some(checksum) => checksum,
        None => {
            reader.seek(std::io::SeekFrom::Start(0))?;
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 { break; }
                hasher.update(&buffer[..count]);
            }
            content_address::ContentHash::new(hasher.finalize().to_hex().as_str())
        }
    };
    let tracks = audiobook_folder::archived_tracks(&mut reader)?;
    let mut archive = zip::ZipArchive::new(reader)?;
    let mut chapters = Vec::with_capacity(tracks.len());
    let mut track_index = Vec::with_capacity(tracks.len());
    let mut embedded = Vec::with_capacity(tracks.len());
    let mut total = 0u64;
    for (index, track) in tracks.iter().enumerate() {
        #[cfg(not(target_arch = "wasm32"))]
        let facts = {
            let mut audio = tempfile::tempfile()?;
            std::io::copy(&mut archive.by_index(index)?, &mut audio)?;
            audio.rewind()?;
            audiobook_folder::inspect_mp3(audio, false)?
        };
        #[cfg(target_arch = "wasm32")]
        let facts = {
            let mut bytes = Vec::new();
            archive.by_index(index)?.read_to_end(&mut bytes)?;
            audiobook_folder::inspect_mp3(std::io::Cursor::new(bytes), false)?
        };
        let duration = facts.duration_ms;
        let stem = Path::new(&track.name).file_stem().and_then(|value| value.to_str()).unwrap_or(&track.name);
        let title = facts.track_title.as_deref().filter(|title| !title.trim().is_empty()).map(str::to_owned).unwrap_or_else(|| book_model::audiobook_chapter_display_title(stem, index + 1));
        let end = total.checked_add(duration).ok_or("audiobook duration overflow")?;
        chapters.push(AudiobookChapterMetadata { title, start_ms: total, end_ms: end });
        track_index.push(book_model::AudiobookTrack { name: track.name.clone(), start_ms: total, end_ms: end, offset: track.offset, length: track.length, archive_checksum: Some(checksum) });
        embedded.push(facts);
        total = end;
    }
    let default_title = source_name.file_stem().and_then(|value| value.to_str()).unwrap_or("Untitled").to_owned();
    let mut audiobook = AudiobookMetadata { title: default_title, authors: Vec::new(), description: String::new(), duration_ms: total, chapters, tracks: track_index, chapter_origin: ChapterOrigin::TrackFiles, recording: Default::default() };
    if let Some(album) = embedded.iter().find_map(|facts| facts.album.as_deref()) { audiobook.title = album.to_owned(); }
    if let Some(author) = embedded.iter().find_map(|facts| facts.album_artist.as_deref()).or_else(|| embedded.iter().find_map(|facts| facts.artist.as_deref())) {
        audiobook.authors = author.split(';').filter_map(|name| author_credit(name.trim().to_owned()).ok()).collect();
    }
    if let Some(description) = embedded.iter().find_map(|facts| facts.description.as_deref()) { audiobook.description = description.to_owned(); }
    if let Some(narrator) = embedded.iter().find_map(|facts| facts.narrator.as_deref()) { audiobook.recording.narrators.push(narrator.to_owned()); }
    if let Some(publisher) = embedded.iter().find_map(|facts| facts.publisher.as_deref()) { audiobook.recording.publishers.push(publisher.to_owned()); }
    if let Some(asin) = embedded.iter().find_map(|facts| facts.asin.as_deref()).filter(|value| value.len() == 10 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())) { audiobook.recording.asins.push(asin.to_ascii_uppercase()); }
    if let Some(isbn) = embedded.iter().find_map(|facts| facts.isbn.as_deref()).and_then(book_model::from_metadata_value) { audiobook.recording.isbns.push(isbn); }
    if let Some(date) = embedded.iter().find_map(|facts| facts.release_date.as_deref()) {
        if date.len() == 4 && date.bytes().all(|byte| byte.is_ascii_digit()) {
            audiobook.recording.recording_release_year = date.parse::<i32>().ok().filter(|year| (1000..=2100).contains(year));
        }
        else { audiobook.recording.recording_release_date = Some(date.to_owned()); }
    }
    let mut book = recording_book(&audiobook.recording)?;
    for index in tracks.len()..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_ascii_lowercase();
        if !name.ends_with(".nfo") && !name.ends_with(".cue") { continue; }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if name.ends_with(".nfo") {
            if bytes.len() as u64 > audiobook_folder::MAX_NFO_BYTES { return Err("audiobook NFO is too large".into()); }
            book = nfo::apply(&decode_sidecar(&bytes), &mut audiobook)?;
        } else if name.ends_with(".cue") {
            if bytes.len() as u64 > audiobook_folder::MAX_CUE_BYTES { return Err("audiobook CUE is too large".into()); }
            match cue::chapters(&decode_sidecar(&bytes), &audiobook.tracks) {
                Ok(chapters) => { audiobook.chapters = chapters; audiobook.chapter_origin = ChapterOrigin::CueSheet; },
                Err(error) => log::warn!("ignoring invalid audiobook CUE: {error}"),
            }
        }
    }
    Ok((audiobook, book))
}

fn decode_sidecar(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.trim_start_matches('\u{feff}').to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// Inspects picker or network bytes through the shared reader contract.
pub fn inspect_book_bytes(source_name: &str, format: BookFormat, bytes: Vec<u8>) -> MetadataResult<InspectedBook> {
    inspect_book_reader(Path::new(source_name), format, std::io::Cursor::new(bytes))
}

/// Best-effort metadata inspection for a local library scan.
///
/// Import, upload, and reader-open paths use [`inspect_book`] and retain its
/// strict validation. A scanner must still index a supported local file when
/// its embedded metadata is malformed or unavailable, so it falls back to the
/// filename and leaves format validation to the eventual open operation.
pub fn inspect_book_for_library(path: &Path, format: BookFormat) -> InspectedBook {
    inspect_book(path, format).unwrap_or_else(|_| InspectedBook { metadata: fallback_metadata(path), audiobook: None, toc: Vec::new(), pdf: None })
}

fn fallback_metadata(path: &Path) -> BookRecord {
    let title = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("Untitled").to_owned();
    let mut metadata = BookRecord { title, subtitle: None, contributors: Vec::new(), description: String::new(), book: Default::default() };
    metadata.normalize_title();
    metadata
}

fn author_credit(name: String) -> Result<Contributor, BookMetadataError> {
    Contributor::new(name, AUTHOR_MARC_RELATOR_CODE)
}

fn m4b_metadata_from_tag(tag: mp4ameta::Tag) -> MetadataResult<M4bMetadata> {
    // An empty title here is intentional: `normalize_with_filename` supplies a
    // proper title from the source filename, more thoroughly than a bare stem.
    let title = tag.title().or_else(|| tag.album()).map(str::trim).filter(|title| !title.is_empty()).map(str::to_owned).unwrap_or_default();
    let authors = m4b_authors(&tag);

    let description = tag.description().map(str::trim).unwrap_or_default().to_owned();
    let duration = tag.duration();
    if duration.is_zero() {
        return Err("M4B book contains no playable audio duration".into());
    }
    let (chapters, chapter_origin) = normalize_m4b_chapters(tag.chapters(), duration);
    let recording = recording_evidence(&tag, &authors);
    Ok(M4bMetadata { title, authors, description, duration, chapters, chapter_origin, recording })
}

fn audiobook_metadata_from(tag: mp4ameta::Tag) -> MetadataResult<AudiobookMetadata> {
    audiobook_metadata_from_m4b(m4b_metadata_from_tag(tag)?)
}

fn audiobook_metadata_from_m4b(metadata: M4bMetadata) -> MetadataResult<AudiobookMetadata> {
    let authors = metadata.authors.into_iter().map(|author| author_credit(author)).collect::<Result<Vec<_>, _>>()?;
    let chapters = metadata.chapters.into_iter().map(|chapter| AudiobookChapterMetadata { title: chapter.title, start_ms: duration_ms(chapter.start), end_ms: duration_ms(chapter.end) }).collect();
    Ok(AudiobookMetadata { title: metadata.title, authors, description: metadata.description, duration_ms: duration_ms(metadata.duration), chapters, tracks: Vec::new(), chapter_origin: metadata.chapter_origin, recording: metadata.recording })
}

fn recording_book(evidence: &metadata_contract::audible::RecordingEvidence) -> MetadataResult<book_model::BookMetadata> {
    use book_model::{Identifier, Scheme, Scope};
    let mut book = book_model::BookMetadata::default();
    book.publishers = evidence.publishers.iter().filter_map(|name| book_model::PublisherCredit::new(name).ok()).collect();
    for (values, scheme) in [(&evidence.isbns, Scheme::Isbn), (&evidence.asins, Scheme::Asin)] {
        for value in values {
            book.identifiers.push(Identifier::new(value, scheme.clone(), Scope::Edition)?);
        }
    }
    Ok(book)
}

fn recording_evidence(tag: &mp4ameta::Tag, authors: &[String]) -> metadata_contract::audible::RecordingEvidence {
    fn values(tag: &mp4ameta::Tag, names: &[&str]) -> Vec<String> {
        let mut values = Vec::new();
        for name in names {
            let id = mp4ameta::FreeformIdent::new_borrowed("com.apple.iTunes", name);
            for value in tag.strings_of(&id).map(str::trim).filter(|v| !v.is_empty()) {
                if !values.iter().any(|existing| existing == value) {
                    values.push(value.to_owned());
                }
            }
        }
        values
    }
    let asins = values(tag, &["ASIN", "asin", "AUDIBLE_ASIN"]).into_iter().map(|value| value.to_ascii_uppercase()).filter(|value| value.len() == 10 && value.bytes().all(|b| b.is_ascii_alphanumeric())).collect();
    let isbns = values(tag, &["ISBN", "isbn", "ISBN13", "ISBN-13", "ISBN10"]).into_iter().filter_map(|value| book_model::from_metadata_value(&value)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    // Explicit NARRATOR atoms may name more than one person; only when none
    // exist does the composer fallback apply, and it never relabels an author.
    let mut narrators = values(tag, &["NARRATOR", "narrator"]);
    if narrators.is_empty() {
        narrators.extend(composer_narrator(tag, authors));
    }
    metadata_contract::audible::RecordingEvidence {
        album: tag.album().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned),
        subtitle: values(tag, &["SUBTITLE", "subtitle"]).into_iter().next(),
        narrators,
        asins,
        isbns,
        publishers: values(tag, &["PUBLISHER", "publisher"]),
        date: tag.year().map(str::to_owned),
        recording_release_date: values(tag, &["AUDIBLE_RELEASE_DATE", "RECORDING_RELEASE_DATE"]).into_iter().next(),
        recording_release_year: None,
    }
}

/// Who wrote it. An explicit AUTHOR atom outranks `artist`, because `artist` is
/// where a producer puts whichever name it considers the performer — sometimes
/// the writer, sometimes the narrator. Where it is absent the older order
/// stands: artist, then album artist.
fn m4b_authors(tag: &mp4ameta::Tag) -> Vec<String> {
    let mut authors = freeform_values(tag, &["AUTHOR", "author"]);
    if authors.is_empty() {
        authors.extend(tag.artists().map(str::trim).filter(|author| !author.is_empty()).map(str::to_owned));
    }
    if authors.is_empty() {
        authors.extend(tag.album_artists().map(str::trim).filter(|author| !author.is_empty()).map(str::to_owned));
    }
    let mut seen = HashSet::new();
    authors.retain(|author| seen.insert(author.clone()));
    authors
}

/// Non-empty values of one or more freeform iTunes atoms, in the order asked
/// for and without repeats.
fn freeform_values(tag: &mp4ameta::Tag, names: &[&str]) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for name in names {
        let id = mp4ameta::FreeformIdent::new_borrowed("com.apple.iTunes", name);
        for value in tag.strings_of(&id).map(str::trim).filter(|value| !value.is_empty()) {
            if !values.iter().any(|existing| existing == value) {
                values.push(value.to_owned());
            }
        }
    }
    values
}

/// `composer` is the usual home for a narrator in audiobooks that carry no
/// freeform atom — but a good many files repeat the author there instead, so it
/// is only believed when it names somebody else. Reading it unconditionally
/// would credit an author as their own narrator.
fn composer_narrator(tag: &mp4ameta::Tag, authors: &[String]) -> Option<String> {
    let composer = tag.composers().map(str::trim).find(|value| !value.is_empty())?;
    let repeats_an_author = authors.iter().any(|author| author.eq_ignore_ascii_case(composer));
    (!repeats_an_author).then(|| composer.to_owned())
}

fn normalize_m4b_chapters(embedded: &[mp4ameta::Chapter], duration: Duration) -> (Vec<book_model::AudiobookChapter>, ChapterOrigin) {
    let full_audiobook = || vec![book_model::AudiobookChapter { title: "Full audiobook".to_owned(), start: Duration::ZERO, end: duration }];
    let mut normalized = embedded.iter().filter(|chapter| chapter.start < duration).map(|chapter| (chapter.start, (!chapter.title.trim().is_empty()).then(|| chapter.title.trim().to_owned()))).collect::<Vec<_>>();
    if normalized.is_empty() {
        return (full_audiobook(), ChapterOrigin::FullAudiobookFallback);
    }
    normalized.sort_by_key(|(start, _)| *start);
    normalized.dedup_by(|current, previous| current.0 == previous.0);
    normalized[0].0 = Duration::ZERO;
    let chapters = normalized
        .iter()
        .enumerate()
        .map(|(index, (start, title))| book_model::AudiobookChapter { title: title.clone().unwrap_or_else(|| format!("Chapter {}", index + 1)), start: *start, end: normalized.get(index + 1).map(|(next, _)| *next).unwrap_or(duration) })
        .collect();
    (chapters, ChapterOrigin::EmbeddedM4b)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(title: &str, start: u64, end: u64) -> book_model::AudiobookChapter {
        book_model::AudiobookChapter { title: title.to_owned(), start: Duration::from_secs(start), end: Duration::from_secs(end) }
    }

    #[test]
    fn m4b_chapters_form_a_complete_non_empty_timeline() {
        let embedded = vec![mp4ameta::Chapter::new(Duration::from_secs(50), "  Last  "), mp4ameta::Chapter::new(Duration::from_secs(10), "  "), mp4ameta::Chapter::new(Duration::from_secs(120), "Past end")];

        let (chapters, origin) = normalize_m4b_chapters(&embedded, Duration::from_secs(100));

        assert_eq!(origin, ChapterOrigin::EmbeddedM4b);
        assert_eq!(chapters, vec![chapter("Chapter 1", 0, 50), chapter("Last", 50, 100)]);
    }

    #[test]
    fn invalid_m4b_chapters_use_full_book_fallback() {
        let embedded = vec![mp4ameta::Chapter::new(Duration::from_secs(100), "At end"), mp4ameta::Chapter::new(Duration::from_secs(200), "Past end")];

        let (chapters, origin) = normalize_m4b_chapters(&embedded, Duration::from_secs(100));

        assert_eq!(origin, ChapterOrigin::FullAudiobookFallback);
        assert_eq!(chapters, vec![chapter("Full audiobook", 0, 100)]);

        let (chapters, origin) = normalize_m4b_chapters(&[], Duration::from_secs(100));
        assert_eq!(origin, ChapterOrigin::FullAudiobookFallback);
        assert_eq!(chapters, vec![chapter("Full audiobook", 0, 100)]);
    }

    #[test]
    fn duplicate_m4b_chapter_starts_are_collapsed() {
        let embedded = vec![mp4ameta::Chapter::new(Duration::from_secs(10), "First"), mp4ameta::Chapter::new(Duration::from_secs(10), "Duplicate"), mp4ameta::Chapter::new(Duration::from_secs(60), "Second")];

        let (chapters, origin) = normalize_m4b_chapters(&embedded, Duration::from_secs(100));

        assert_eq!(origin, ChapterOrigin::EmbeddedM4b);
        assert_eq!(chapters, vec![chapter("First", 0, 60), chapter("Second", 60, 100)]);
    }

    #[test]
    fn library_inspection_falls_back_when_embedded_metadata_is_invalid() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Local title.pdf");
        std::fs::write(&path, b"not a valid PDF").unwrap();

        let inspected = inspect_book_for_library(&path, BookFormat::Pdf);

        assert_eq!(inspected.metadata.title, "Local title");
        assert!(inspected.audiobook.is_none());
    }

    #[test]
    fn mp3_folder_archive_inspection_builds_a_book_timeline() {
        let directory = tempfile::tempdir().unwrap();
        let sample = include_bytes!("../tests/fixtures/silence.mp3");
        std::fs::write(directory.path().join("01.mp3"), sample).unwrap();
        std::fs::write(directory.path().join("02.mp3"), sample).unwrap();
        std::fs::write(directory.path().join("book.nfo"), "Title: NFO Title\nAuthor: Test Author\nRead by: Test Narrator\nASIN: B0DWXY6C69\nISBN: 9780306406157\nLanguage: en\nGenre: Fiction\n\nBook Description\n================\nThe description.\n").unwrap();
        let tracks = audiobook_folder::discover(directory.path()).unwrap();
        let nfo = audiobook_folder::discover_nfo(directory.path()).unwrap();
        let mut archive = audiobook_folder::write_archive_with_nfo(tempfile::tempfile().unwrap(), &tracks, nfo.as_ref()).unwrap();
        archive.rewind().unwrap();
        let mut bytes = Vec::new();
        archive.read_to_end(&mut bytes).unwrap();
        archive.rewind().unwrap();
        let checksum = content_address::ContentHash::new(blake3::hash(&bytes).to_hex().as_str());

        let inspected = inspect_book_reader(Path::new("Sample Book.mp3folder"), BookFormat::Mp3Folder, archive).unwrap();
        let audio = inspected.audiobook.unwrap();
        assert!(book_model::tracks_match_archive(&audio.tracks, checksum));
        assert_eq!(inspected.metadata.title, "NFO Title");
        assert_eq!(inspected.metadata.description, "The description.");
        assert_eq!(inspected.metadata.contributors.len(), 1);
        assert_eq!(inspected.metadata.book.identifiers.len(), 2);
        assert_eq!(inspected.metadata.book.languages.len(), 1);
        assert_eq!(inspected.metadata.book.subjects.len(), 1);
        assert_eq!(audio.chapter_origin, ChapterOrigin::TrackFiles);
        assert_eq!(audio.tracks.len(), 2);
        assert_eq!(audio.tracks[0].start_ms, 0);
        assert!(audio.tracks[0].end_ms >= 500);
        assert_eq!(audio.tracks[1].start_ms, audio.tracks[0].end_ms);
        assert_eq!(audio.chapters[1].start_ms, audio.tracks[1].start_ms);
        assert_eq!(audio.duration_ms, audio.tracks[1].end_ms);
        assert_eq!(audio.tracks[0].length, sample.len() as u64);
        assert_eq!(audio.recording.asins, ["B0DWXY6C69"]);
        assert_eq!(audio.recording.narrators, ["Test Narrator"]);
    }

    #[test]
    fn mp3_folder_cue_replaces_file_chapters_without_changing_track_index() {
        let directory = tempfile::tempdir().unwrap();
        let sample = include_bytes!("../tests/fixtures/silence.mp3");
        std::fs::write(directory.path().join("01.mp3"), sample).unwrap();
        std::fs::write(directory.path().join("book.cue"), "FILE \"01.mp3\" MP3\nTRACK 01 AUDIO\nTITLE \"First\"\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nTITLE \"Second\"\nINDEX 01 00:00:20\n").unwrap();
        let tracks = audiobook_folder::discover(directory.path()).unwrap();
        let cue = audiobook_folder::discover_cue(directory.path()).unwrap();
        let archive = audiobook_folder::write_archive_with_sidecars(tempfile::tempfile().unwrap(), &tracks, None, cue.as_ref(), None).unwrap();
        let inspected = inspect_book_reader(Path::new("Sample Book.mp3folder"), BookFormat::Mp3Folder, archive).unwrap();
        let audio = inspected.audiobook.unwrap();
        assert_eq!(audio.chapter_origin, ChapterOrigin::CueSheet);
        assert_eq!(audio.tracks.len(), 1);
        assert_eq!(audio.chapters.len(), 2);
        assert_eq!(audio.chapters[0].end_ms, audio.chapters[1].start_ms);
        assert_eq!(audio.chapters[1].end_ms, audio.duration_ms);
    }

    #[test]
    fn mp3_folder_cue_file_order_matches_playback_track_order() {
        let directory = tempfile::tempdir().unwrap();
        let sample = include_bytes!("../tests/fixtures/silence.mp3");
        std::fs::write(directory.path().join("A.mp3"), sample).unwrap();
        std::fs::write(directory.path().join("B.mp3"), sample).unwrap();
        std::fs::write(directory.path().join("book.cue"), "FILE \"B.mp3\" MP3\nTRACK 01 AUDIO\nTITLE \"First\"\nINDEX 01 00:00:00\nFILE \"A.mp3\" MP3\nTRACK 02 AUDIO\nTITLE \"Second\"\nINDEX 01 00:00:00\n").unwrap();
        let tracks = audiobook_folder::discover(directory.path()).unwrap();
        assert_eq!(tracks.iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["B.mp3", "A.mp3"]);
        let cue = audiobook_folder::discover_cue(directory.path()).unwrap();
        let archive = audiobook_folder::write_archive_with_sidecars(tempfile::tempfile().unwrap(), &tracks, None, cue.as_ref(), None).unwrap();
        let audio = inspect_book_reader(Path::new("Sample Book.mp3folder"), BookFormat::Mp3Folder, archive).unwrap().audiobook.unwrap();
        assert_eq!(audio.chapter_origin, ChapterOrigin::CueSheet);
        assert_eq!(audio.chapters.iter().map(|chapter| chapter.title.as_str()).collect::<Vec<_>>(), ["First", "Second"]);
        assert_eq!(audio.chapters[1].start_ms, audio.tracks[1].start_ms);
    }

    #[test]
    fn mp3_folder_uses_embedded_album_and_track_tags_without_nfo() {
        let directory = tempfile::tempdir().unwrap();
        let mut frames = Vec::new();
        for (key, value) in [("TALB", "Tagged Book"), ("TPE2", "Tagged Author"), ("TIT2", "Tagged Chapter")] {
            frames.extend_from_slice(key.as_bytes());
            frames.extend_from_slice(&((value.len() + 1) as u32).to_be_bytes());
            frames.extend_from_slice(&[0, 0, 0]);
            frames.extend_from_slice(value.as_bytes());
        }
        let size = frames.len() as u32;
        let mut bytes = vec![b'I', b'D', b'3', 3, 0, 0, ((size >> 21) & 0x7f) as u8, ((size >> 14) & 0x7f) as u8, ((size >> 7) & 0x7f) as u8, (size & 0x7f) as u8];
        bytes.extend(frames);
        bytes.extend_from_slice(include_bytes!("../tests/fixtures/silence.mp3"));
        std::fs::write(directory.path().join("01.mp3"), bytes).unwrap();
        let tracks = audiobook_folder::discover(directory.path()).unwrap();
        let archive = audiobook_folder::write_archive(tempfile::tempfile().unwrap(), &tracks).unwrap();
        let inspected = inspect_book_reader(Path::new("Folder.mp3folder"), BookFormat::Mp3Folder, archive).unwrap();
        let audio = inspected.audiobook.unwrap();
        assert_eq!(audio.title, "Tagged Book");
        assert_eq!(audio.authors[0].name(), "Tagged Author");
        assert_eq!(audio.chapters[0].title, "Tagged Chapter");
    }
}

#[cfg(test)]
mod recording_evidence_tests {
    use super::*;
    fn set(tag: &mut mp4ameta::Tag, name: &'static str, value: &str) {
        tag.set_data(mp4ameta::FreeformIdent::new_static("com.apple.iTunes", name), mp4ameta::Data::Utf8(value.into()));
    }

    #[test]
    fn recording_evidence_keeps_explicit_identity_and_separates_dates() {
        let mut tag = mp4ameta::Tag::default();
        tag.set_album("Full title: Subtitle");
        tag.set_year("1859");
        set(&mut tag, "ASIN", "b0dwxy6c69");
        set(&mut tag, "ISBN", "0-306-40615-2");
        set(&mut tag, "ISBN13", "9780306406157");
        set(&mut tag, "SUBTITLE", "Subtitle");
        set(&mut tag, "NARRATOR", "Narrator Name");
        set(&mut tag, "PUBLISHER", "Explicit Publisher");
        let evidence = recording_evidence(&tag, &[]);
        assert_eq!(evidence.asins, ["B0DWXY6C69"]);
        assert_eq!(evidence.isbns, ["0306406152", "9780306406157"]);
        assert_eq!(evidence.narrators, ["Narrator Name"]);
        assert_eq!(evidence.publishers, ["Explicit Publisher"]);
        assert_eq!(evidence.date.as_deref(), Some("1859"));
        assert_eq!(evidence.recording_release_date, None, "a generic date must not become recording evidence");
        assert_eq!(evidence.subtitle.as_deref(), Some("Subtitle"));
        let book = recording_book(&evidence).unwrap();
        assert_eq!(book.identifiers.len(), 3);
        assert_eq!(book.identifiers.iter().filter(|id| id.scheme() == &book_model::Scheme::Isbn).count(), 2);
    }

    #[test]
    fn recording_evidence_does_not_infer_publisher_or_accept_invalid_identifiers() {
        let mut tag = mp4ameta::Tag::default();
        tag.set_description("Penguin presents an audiobook");
        set(&mut tag, "ASIN", "not an asin");
        set(&mut tag, "ISBN", "9780306406158");
        let evidence = recording_evidence(&tag, &[]);
        assert!(evidence.asins.is_empty());
        assert!(evidence.isbns.is_empty());
        assert!(evidence.publishers.is_empty());
    }

    #[test]
    fn recording_evidence_uses_metadata_value_rules_for_isbn_atoms() {
        let mut tag = mp4ameta::Tag::default();
        set(&mut tag, "ISBN13", "ISBN: 978‑0‑13‑110362‑7");
        set(&mut tag, "ISBN", "0000000000");
        set(&mut tag, "ISBN10", "9770131103627");
        let evidence = recording_evidence(&tag, &[]);
        assert_eq!(evidence.isbns, ["9780131103627"]);
    }
}

/// M4B has no role system: a name means whatever atom it happens to sit in, and
/// producers disagree. These cases are taken from real files — one puts the
/// author in `composer`, another puts the narrator there, a third states both
/// in freeform atoms and leaves `artist` holding the publisher.
///
/// The helpers are exercised directly because a tag's duration comes from the
/// audio track and cannot be set on a synthetic one.
#[cfg(test)]
mod m4b_credit_tests {
    use super::*;

    fn set(tag: &mut mp4ameta::Tag, name: &'static str, value: &str) {
        tag.set_data(mp4ameta::FreeformIdent::new_static("com.apple.iTunes", name), mp4ameta::Data::Utf8(value.into()));
    }

    fn narrator(tag: &mp4ameta::Tag) -> Option<String> {
        let authors = m4b_authors(tag);
        recording_evidence(tag, &authors).narrators.into_iter().next()
    }

    #[test]
    fn m4b_credit_precedence_and_publisher_fallback() {
        let mut tag = mp4ameta::Tag::default();
        tag.set_artist("Historiska media");
        set(&mut tag, "AUTHOR", "Olle Larsson");
        set(&mut tag, "NARRATOR", "Per Juhlin");

        assert_eq!(m4b_authors(&tag), ["Olle Larsson"], "the stated author beats whoever is in `artist`");
        assert_eq!(narrator(&tag).as_deref(), Some("Per Juhlin"));

        let mut tag = mp4ameta::Tag::default();
        tag.set_artist("Chris Wickham");
        tag.set_composer("James Cameron Stewart");

        assert_eq!(m4b_authors(&tag), ["Chris Wickham"]);
        assert_eq!(narrator(&tag).as_deref(), Some("James Cameron Stewart"), "the convention holds when the two names differ");

        let mut tag = mp4ameta::Tag::default();
        tag.set_artist("Norah Vincent");
        tag.set_composer("norah vincent");

        assert_eq!(m4b_authors(&tag), ["Norah Vincent"]);
        assert_eq!(narrator(&tag), None, "an author must not be credited as their own narrator");

        let mut tag = mp4ameta::Tag::default();
        tag.set_album_artist("Fallback Author");

        assert_eq!(m4b_authors(&tag), ["Fallback Author"]);

        let mut tag = mp4ameta::Tag::default();
        set(&mut tag, "PUBLISHER", "Historiska media");

        let book = recording_book(&recording_evidence(&tag, &[])).unwrap();

        assert_eq!(book.publishers.len(), 1);
        assert_eq!(book.publishers[0].name().as_str(), "Historiska media");
    }
}
