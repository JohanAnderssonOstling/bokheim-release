//! Inspects any supported book format, audiobooks included, into the typed
//! record the library stores. EPUB, MOBI and PDF go through this crate's
//! document inspection; M4B and MP3-folder audiobooks are read here.

use crate::{MetadataResult, inspect_epub_reader, inspect_pdf_reader};
use book_content::{Book, BookFormat as ContainerFormat, TocEntry, comic::ComicBook};
use book_model::{AUTHOR_MARC_RELATOR_CODE, AudiobookChapter, BookFormat, BookMetadataError, BookRecord, ChapterOrigin, Contributor};
use content_address::{hash_file, BookId};
use std::collections::HashSet;
#[cfg(target_arch = "wasm32")]
use std::io::Cursor;
use std::io::{Read, Seek};
use std::path::Path;
use std::time::Duration;

mod cue;
mod nfo;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudiobookMetadata {
    pub title: String,
    pub authors: Vec<Contributor>,
    pub description: String,
    pub duration_ms: u64,
    pub chapters: Vec<AudiobookChapter>,
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
            chapters: self.chapters.clone(),
        }
    }
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

/// Inspects a seekable book using the same validation on every platform.
pub fn inspect_book_reader(source_name: &Path, format: BookFormat, reader: impl Read + Seek + Send + Sync + 'static) -> MetadataResult<InspectedBook> {
    inspect_book_reader_with_hash(source_name, format, reader, None)
}

pub fn inspect_book_reader_with_hash(source_name: &Path, format: BookFormat, reader: impl Read + Seek + Send + Sync + 'static, content_hash: Option<BookId>) -> MetadataResult<InspectedBook> {
    let mut pdf = None;
    let mut pdf_toc = Vec::new();
    let reader = if format == BookFormat::Pdf {
        let (reader, extracted, outline) = match content_hash {
            Some(identity) if identity.is_external() => pdf_reader_core::inspect_reader_metadata_with_identity(reader, identity.to_hex())?,
            _ => pdf_reader_core::inspect_reader_metadata(reader)?,
        };
        pdf = extracted.valid_for(&extracted.content_hash).then_some(extracted);
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
        BookFormat::Mobi => {
            let (metadata, toc) = crate::inspect_mobi_reader(source_name, reader)?;
            (metadata, None, toc)
        }
        BookFormat::Fb2 => {
            let mut book = Book::open_reader(ContainerFormat::Fb2, reader)?;
            let metadata = crate::metadata_from_book(source_name, &mut book)?;
            let toc = crate::book_toc_entries(book.toc()?.unwrap_or_default());
            (metadata, None, toc)
        }
        BookFormat::Cbz => {
            let book = ComicBook::open_reader(reader)?;
            let metadata = crate::metadata_from_declared(source_name, book.metadata)?;
            let toc = crate::book_toc_entries(book.pages.iter().enumerate().filter_map(|(index, page)| page.bookmark.as_ref().map(|title| TocEntry {
                title: title.clone(), link: book_model::cbz_toc_target(index), children: Vec::new(),
            })).collect());
            (metadata, None, toc)
        }
        BookFormat::Pdf => (inspect_pdf_reader(source_name, reader)?.metadata, None, pdf_toc),
        BookFormat::M4b => {
            let tag = mp4ameta::Tag::read_from(&mut reader)?;
            // An empty title here is intentional: `normalize_with_filename` supplies a
            // proper title from the source filename, more thoroughly than a bare stem.
            let title = tag.title().or_else(|| tag.album()).map(str::trim).filter(|title| !title.is_empty()).map(str::to_owned).unwrap_or_default();
            let authors = {
                let mut authors = freeform_values(&tag, &["AUTHOR", "author"]);
                if authors.is_empty() {
                    authors.extend(tag.artists().map(str::trim).filter(|author| !author.is_empty()).map(str::to_owned));
                }
                if authors.is_empty() {
                    authors.extend(tag.album_artists().map(str::trim).filter(|author| !author.is_empty()).map(str::to_owned));
                }
                let mut seen = HashSet::new();
                authors.retain(|author| seen.insert(author.clone()));
                authors
            };
            let description = tag.description().map(str::trim).unwrap_or_default().to_owned();
            let duration = tag.duration();
            if duration.is_zero() {
                return Err("M4B book contains no playable audio duration".into());
            }
            let mut normalized = tag.chapters().iter().filter(|chapter| chapter.start < duration).map(|chapter| (chapter.start, (!chapter.title.trim().is_empty()).then(|| chapter.title.trim().to_owned()))).collect::<Vec<_>>();
            let (chapters, chapter_origin) = if normalized.is_empty() {
                (vec![AudiobookChapter { title: "Full audiobook".to_owned(), start_ms: 0, end_ms: duration_ms(duration) }], ChapterOrigin::FullAudiobookFallback)
            } else {
                normalized.sort_by_key(|(start, _)| *start);
                normalized.dedup_by(|current, previous| current.0 == previous.0);
                normalized[0].0 = Duration::ZERO;
                let chapters = normalized
                    .iter()
                    .enumerate()
                    .map(|(index, (start, title))| AudiobookChapter {
                        title: title.clone().unwrap_or_else(|| format!("Chapter {}", index + 1)),
                        start_ms: duration_ms(*start),
                        end_ms: duration_ms(normalized.get(index + 1).map(|(next, _)| *next).unwrap_or(duration)),
                    })
                    .collect();
                (chapters, ChapterOrigin::EmbeddedM4b)
            };
            let recording = {
                let asins = freeform_values(&tag, &["ASIN", "asin", "AUDIBLE_ASIN"]).into_iter().map(|value| value.to_ascii_uppercase()).filter(|value| value.len() == 10 && value.bytes().all(|b| b.is_ascii_alphanumeric())).collect();
                let isbns = freeform_values(&tag, &["ISBN", "isbn", "ISBN13", "ISBN-13", "ISBN10"]).into_iter().filter_map(|value| book_model::from_metadata_value(&value)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                // Explicit NARRATOR atoms may name more than one person; only when none
                // exist does the composer fallback apply, and it never relabels an author.
                let mut narrators = freeform_values(&tag, &["NARRATOR", "narrator"]);
                if narrators.is_empty() {
                    // `composer` is the usual home for a narrator in audiobooks that carry no
                    // freeform atom — but a good many files repeat the author there instead,
                    // so it is only believed when it names somebody else. Reading it
                    // unconditionally would credit an author as their own narrator.
                    if let Some(composer) = tag.composers().map(str::trim).find(|value| !value.is_empty()) {
                        if !authors.iter().any(|author| author.eq_ignore_ascii_case(composer)) {
                            narrators.push(composer.to_owned());
                        }
                    }
                }
                metadata_contract::audible::RecordingEvidence {
                    album: tag.album().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned),
                    subtitle: freeform_values(&tag, &["SUBTITLE", "subtitle"]).into_iter().next(),
                    narrators,
                    asins,
                    isbns,
                    publishers: freeform_values(&tag, &["PUBLISHER", "publisher"]),
                    date: tag.year().map(str::to_owned),
                    recording_release_date: freeform_values(&tag, &["AUDIBLE_RELEASE_DATE", "RECORDING_RELEASE_DATE"]).into_iter().next(),
                    recording_release_year: None,
                }
            };
            let audiobook = AudiobookMetadata {
                title,
                authors: authors.into_iter().map(author_credit).collect::<Result<Vec<_>, _>>()?,
                description,
                duration_ms: duration_ms(duration),
                chapters,
                tracks: Vec::new(),
                chapter_origin,
                recording,
            };
            let subtitle = audiobook.recording.subtitle.as_deref().filter(|value| !value.trim().is_empty()).map(str::to_owned);
            let metadata = BookRecord {
                metadata: book_model::BookDetails { title: audiobook.title.clone(), subtitle, contributors: audiobook.authors.clone(), book: recording_book(&audiobook.recording)? },
                description: audiobook.description.clone(),
            };
            (metadata, Some(audiobook), Vec::new())
        }
        BookFormat::Mp3Folder => {
            let (audiobook, book) = {
                let content_hash = match content_hash {
                    Some(content_hash) => content_hash,
                    None => BookId::from(hash_file(&mut reader)?),
                };
                let tracks = audiobook_archive::archived_tracks(&mut reader)?;
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
                        audiobook_folder::inspect_mp3(Cursor::new(bytes), false)?
                    };
                    let duration = facts.duration_ms;
                    let stem = Path::new(&track.name).file_stem().and_then(|value| value.to_str()).unwrap_or(&track.name);
                    let title = facts.track_title.as_deref().filter(|title| !title.trim().is_empty()).map(str::to_owned).unwrap_or_else(|| book_model::audiobook_chapter_display_title(stem, index + 1));
                    let end = total.checked_add(duration).ok_or("audiobook duration overflow")?;
                    chapters.push(AudiobookChapter { title, start_ms: total, end_ms: end });
                    track_index.push(book_model::AudiobookTrack { name: track.name.clone(), start_ms: total, end_ms: end, offset: track.offset, length: track.length, archive_hash: Some(content_hash) });
                    embedded.push(facts);
                    total = end;
                }
                let default_title = source_name.file_stem().and_then(|value| value.to_str()).unwrap_or("Untitled").to_owned();
                let mut audiobook =
                    AudiobookMetadata { title: default_title, authors: Vec::new(), description: String::new(), duration_ms: total, chapters, tracks: track_index, chapter_origin: ChapterOrigin::TrackFiles, recording: Default::default() };
                if let Some(album) = embedded.iter().find_map(|facts| facts.album.as_deref()) {
                    audiobook.title = album.to_owned();
                }
                if let Some(author) = embedded.iter().find_map(|facts| facts.album_artist.as_deref()).or_else(|| embedded.iter().find_map(|facts| facts.artist.as_deref())) {
                    audiobook.authors = author.split(';').filter_map(|name| author_credit(name.trim().to_owned()).ok()).collect();
                }
                if let Some(description) = embedded.iter().find_map(|facts| facts.description.as_deref()) {
                    audiobook.description = description.to_owned();
                }
                if let Some(narrator) = embedded.iter().find_map(|facts| facts.narrator.as_deref()) {
                    audiobook.recording.narrators.push(narrator.to_owned());
                }
                if let Some(publisher) = embedded.iter().find_map(|facts| facts.publisher.as_deref()) {
                    audiobook.recording.publishers.push(publisher.to_owned());
                }
                if let Some(asin) = embedded.iter().find_map(|facts| facts.asin.as_deref()).filter(|value| value.len() == 10 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())) {
                    audiobook.recording.asins.push(asin.to_ascii_uppercase());
                }
                if let Some(isbn) = embedded.iter().find_map(|facts| facts.isbn.as_deref()).and_then(book_model::from_metadata_value) {
                    audiobook.recording.isbns.push(isbn);
                }
                if let Some(date) = embedded.iter().find_map(|facts| facts.release_date.as_deref()) {
                    if date.len() == 4 && date.bytes().all(|byte| byte.is_ascii_digit()) {
                        audiobook.recording.recording_release_year = date.parse::<i32>().ok().filter(|year| (1000..=2100).contains(year));
                    } else {
                        audiobook.recording.recording_release_date = Some(date.to_owned());
                    }
                }
                let mut book = recording_book(&audiobook.recording)?;
                for index in tracks.len()..archive.len() {
                    let mut entry = archive.by_index(index)?;
                    let name = entry.name().to_ascii_lowercase();
                    if !name.ends_with(".nfo") && !name.ends_with(".cue") {
                        continue;
                    }
                    let mut bytes = Vec::new();
                    entry.read_to_end(&mut bytes)?;
                    if name.ends_with(".nfo") {
                        if bytes.len() as u64 > audiobook_archive::MAX_NFO_BYTES {
                            return Err("audiobook NFO is too large".into());
                        }
                        book = nfo::apply(&decode_sidecar(&bytes), &mut audiobook)?;
                    } else if name.ends_with(".cue") {
                        if bytes.len() as u64 > audiobook_archive::MAX_CUE_BYTES {
                            return Err("audiobook CUE is too large".into());
                        }
                        match cue::chapters(&decode_sidecar(&bytes), &audiobook.tracks) {
                            Ok(chapters) => {
                                audiobook.chapters = chapters;
                                audiobook.chapter_origin = ChapterOrigin::CueSheet;
                            }
                            Err(error) => log::warn!("ignoring invalid audiobook CUE: {error}"),
                        }
                    }
                }
                (audiobook, book)
            };
            let metadata = BookRecord {
                metadata: book_model::BookDetails { title: audiobook.title.clone(), subtitle: audiobook.recording.subtitle.clone(), contributors: audiobook.authors.clone(), book },
                description: audiobook.description.clone(),
            };
            (metadata, Some(audiobook), Vec::new())
        }
    };
    crate::normalize_with_filename(&mut metadata, &source_name.to_string_lossy())?;
    Ok(InspectedBook { metadata, audiobook, toc, pdf })
}

fn decode_sidecar(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.trim_start_matches('\u{feff}').to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

fn author_credit(name: String) -> Result<Contributor, BookMetadataError> {
    Contributor::new(name, AUTHOR_MARC_RELATOR_CODE)
}

fn recording_book(evidence: &metadata_contract::audible::RecordingEvidence) -> MetadataResult<book_model::BookMetadata> {
    use book_model::{Identifier, Scheme, Scope};
    let mut book = book_model::BookMetadata::default();
    book.publishers = evidence.publishers.clone();
    for (values, scheme) in [(&evidence.isbns, Scheme::Isbn), (&evidence.asins, Scheme::Asin)] {
        for value in values {
            book.identifiers.push(Identifier::new(value, scheme.clone(), Scope::Edition)?);
        }
    }
    Ok(book)
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

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fb2_import_keeps_book_authors_separate_from_file_creators_and_print_isbn() {
        let xml = br#"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description>
<title-info><book-title>Example novel</book-title><author><first-name>Ada</first-name><last-name>Writer</last-name></author><lang>en</lang></title-info>
<document-info><author><first-name>File</first-name><last-name>Creator</last-name></author></document-info>
<publish-info><isbn>9780306406157</isbn><year>1980</year></publish-info></description><body><section id="chapter"><title><p>First chapter</p></title><p>Book text.</p></section></body></FictionBook>"#;
        let inspected = inspect_book_reader(Path::new("example.fb2"), BookFormat::Fb2, std::io::Cursor::new(xml.to_vec())).unwrap();
        assert_eq!(inspected.metadata.metadata.title, "Example novel");
        assert_eq!(inspected.metadata.metadata.contributors.len(), 1);
        assert_eq!(inspected.metadata.metadata.contributors[0].name(), "Ada Writer");
        assert!(inspected.metadata.book.identifiers.is_empty());
        assert_eq!(inspected.metadata.book.year, None);
        assert_eq!(inspected.toc[0].title, "First chapter");
        assert!(inspected.toc[0].target.starts_with("/fb2/section-0.xhtml#"));
    }

    #[test]
    fn cbz_import_keeps_comicinfo_bookmarks_as_page_targets() {
        use std::io::{Cursor, Write};
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [("1.jpg", b"page".as_slice()), ("2.jpg", b"page".as_slice()), ("ComicInfo.xml", br#"<ComicInfo><Title>Issue One</Title><Writer>Ada Writer</Writer><Pages><Page Image="1" Bookmark="Second part"/></Pages></ComicInfo>"#.as_slice())] {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap(); zip.write_all(bytes).unwrap();
        }
        let inspected = inspect_book_reader(Path::new("issue.cbz"), BookFormat::Cbz, zip.finish().unwrap()).unwrap();
        assert_eq!(inspected.metadata.metadata.title, "Issue One");
        assert_eq!(inspected.metadata.metadata.contributors[0].name(), "Ada Writer");
        assert_eq!(inspected.toc[0].target, book_model::cbz_toc_target(1));
    }

    #[test]
    fn mp3_folder_cue_replaces_file_chapters_without_changing_track_index() {
        let directory = tempfile::tempdir().unwrap();
        let sample = include_bytes!("../tests/fixtures/silence.mp3");
        std::fs::write(directory.path().join("01.mp3"), sample).unwrap();
        std::fs::write(directory.path().join("book.cue"), "FILE \"01.mp3\" MP3\nTRACK 01 AUDIO\nTITLE \"First\"\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nTITLE \"Second\"\nINDEX 01 00:00:20\n").unwrap();
        let tracks = audiobook_folder::discover(directory.path()).unwrap();
        let cue = audiobook_folder::discover_cue(directory.path()).unwrap();
        let archive = audiobook_archive::write_archive_with_sidecars(tempfile::tempfile().unwrap(), &tracks, None, cue.as_ref(), None).unwrap();
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
        let archive = audiobook_archive::write_archive_with_sidecars(tempfile::tempfile().unwrap(), &tracks, None, cue.as_ref(), None).unwrap();
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
        let archive = audiobook_archive::write_archive(tempfile::tempfile().unwrap(), &tracks).unwrap();
        let inspected = inspect_book_reader(Path::new("Folder.mp3folder"), BookFormat::Mp3Folder, archive).unwrap();
        let audio = inspected.audiobook.unwrap();
        assert_eq!(audio.title, "Tagged Book");
        assert_eq!(audio.authors[0].name(), "Tagged Author");
        assert_eq!(audio.chapters[0].title, "Tagged Chapter");
    }
}
