//! Immutable ZIP directory metadata, independent async sources, and owned cursors.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Instant;

use async_zip::base::read1::{
    ZipOptions,
    seek::{ZipArchiveInner, ZipArchiveReader},
};
use futures_lite::io::AsyncReadExt;

use super::{MAX_ARCHIVE_ENTRIES, MAX_BINARY_RESOURCE_BYTES, MAX_PACKAGE_DOCUMENT_BYTES};
use crate::resource_path::{normalize_path, resource_entry};

use crate::profile::{PreparationTiming, ResourceReadTiming, measure};
use crate::source::{ReaderFactory, Source};

pub(crate) struct ArchiveMetadata {
    inner: Arc<ZipArchiveInner>,
    entries: HashMap<String, usize>,
    names: Vec<String>,
}

impl ArchiveMetadata {
    pub(crate) fn record_ranges(&self, names: &[String]) -> io::Result<Vec<(u64, usize)>> {
        let directory = self.inner.ceocdr().cd_offset().map_err(io::Error::other)?;
        let mut offsets = self.inner.cdrs().iter().filter_map(|entry| entry.lfh_offset().ok()).filter(|&offset| offset < directory).collect::<Vec<_>>();
        offsets.push(directory);
        offsets.sort_unstable();
        offsets.dedup();
        let mut ranges = Vec::new();
        for name in names {
            let Ok(index) = self.index(name) else { continue };
            let Ok(start) = self.inner.cdrs()[index].lfh_offset() else { continue };
            let Some(end) = offsets.get(offsets.partition_point(|&offset| offset <= start)).copied().filter(|&end| end <= directory) else { continue };
            let Ok(count) = usize::try_from(end - start) else { continue };
            ranges.push((start, count));
        }
        ranges.sort_unstable();
        ranges.dedup();
        let mut merged: Vec<(u64, usize)> = Vec::new();
        for (start, count) in ranges {
            if let Some((offset, length)) = merged.last_mut() {
                if *offset + *length as u64 == start {
                    *length = length.checked_add(count).ok_or_else(|| io::Error::other("ZIP entry span overflow"))?;
                    continue;
                }
            }
            merged.push((start, count));
        }
        Ok(merged)
    }
    fn new(inner: Arc<ZipArchiveInner>) -> io::Result<Self> {
        let mut entries = HashMap::new();
        let mut names = Vec::new();
        for (index, entry) in inner.cdrs().iter().enumerate() {
            let name = archive_name(entry.insecure_file_name.as_bytes());
            if name.is_empty() || entry.insecure_file_name.as_bytes().ends_with(b"/") {
                continue;
            }
            if !entries.contains_key(&name) {
                entries.insert(name.clone(), index);
                names.push(name);
            }
        }
        Ok(Self { inner, entries, names })
    }
    fn index(&self, uri: &str) -> io::Result<usize> {
        resource_entry(&self.entries, uri).copied().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("EPUB entry not found: {uri}")))
    }
    pub(crate) fn contains(&self, uri: &str) -> bool {
        self.index(uri).is_ok()
    }
    pub(crate) fn resolve(&self, uri: &str) -> Option<String> {
        self.index(uri).ok().map(|index| archive_name(self.inner.cdrs()[index].insecure_file_name.as_bytes()))
    }
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }
    pub(crate) fn size(&self, uri: &str) -> io::Result<u64> {
        self.inner.cdrs()[self.index(uri)?].uncompressed_size().map_err(io::Error::other)
    }
}

fn archive_name(bytes: &[u8]) -> String {
    const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ";
    let name = match std::str::from_utf8(bytes) {
        Ok(name) => name.to_owned(),
        Err(_) => {
            let legacy: Vec<_> = CP437_HIGH.chars().collect();
            bytes.iter().map(|&byte| if byte < 128 { char::from(byte) } else { legacy[usize::from(byte - 128)] }).collect()
        }
    };
    normalize_path(&name.replace('\\', "/"))
}

pub(crate) struct Archive {
    metadata: Arc<ArchiveMetadata>,
    factory: ReaderFactory,
}
impl Archive {
    pub(crate) fn reader(&self) -> ArchiveReader {
        ArchiveReader { metadata: self.metadata.clone(), source: Source::reopenable(self.factory.clone()) }
    }
    pub(crate) async fn read(&self, uri: &str, limit: u64) -> io::Result<Vec<u8>> {
        self.reader().read(uri, limit).await
    }
}

/// A single cursor owned by a sequential inspection or preparation session.
/// The source keeps its buffering and position across ZIP entry decoders.
pub(crate) struct ArchiveReader {
    pub(crate) metadata: Arc<ArchiveMetadata>,
    source: Source,
}
impl ArchiveReader {
    pub(super) fn with_metadata(metadata: Arc<ArchiveMetadata>, source: Source) -> Self {
        Self { metadata, source }
    }
    pub(crate) async fn open(source: Source) -> io::Result<Self> {
        Self::open_with_limit(source, MAX_BINARY_RESOURCE_BYTES.max(MAX_PACKAGE_DOCUMENT_BYTES)).await
    }
    pub(crate) async fn open_with_limit(mut source: Source, limit: u64) -> io::Result<Self> {
        // EPUB exporters often leave stale local headers and checksums. Read
        // using the central directory, enforcing limits on actual decoded data.
        let options = ZipOptions {
            validate_eor_is_eoa: false,
            validate_compressed_size_header_match: false,
            validate_uncompressed_size_header_match: false,
            validate_crc_header_match: false,
            validate_file_name_header_match: false,
            validate_compression_header_match: false,
            validate_gpf_header_match: false,
            validate_num_cd_files: false,
            validate_crc_match_against_read: false,
            validate_uncompressed_size_match_against_read: false,
            validate_file_on_eof: false,
            prefer_central_directory: true,
            max_extra_field_size_per_file: u16::MAX,
            max_extra_field_num_per_file: u16::MAX,
            max_cd_num_files: MAX_ARCHIVE_ENTRIES as u64,
            max_cd_num_files_load: MAX_ARCHIVE_ENTRIES as u64,
            max_uncompressed_size_per_file: limit,
            max_compressed_size_per_file: limit,
            ..ZipOptions::untrusted()
        };
        let reader = ZipArchiveReader::open_with_options(&mut source.reader, options).await.map_err(io::Error::other)?;
        let metadata = Arc::new(ArchiveMetadata::new(reader.inner().clone())?);
        Ok(Self { metadata, source })
    }
    pub(crate) async fn read(&mut self, uri: &str, limit: u64) -> io::Result<Vec<u8>> {
        self.read_measured::<false>(uri, limit).await.map(|(bytes, _)| bytes)
    }
    pub(crate) async fn read_measured<const PROFILE: bool>(&mut self, uri: &str, limit: u64) -> io::Result<(Vec<u8>, ResourceReadTiming)> {
        let (bytes, timing) = self.read_prefix_measured::<PROFILE>(uri, limit.saturating_add(1)).await?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "EPUB resource exceeds the size limit"));
        }
        Ok((bytes, timing))
    }
    pub(crate) async fn read_prefix(&mut self, uri: &str, length: u64) -> io::Result<Vec<u8>> {
        self.read_prefix_measured::<false>(uri, length).await.map(|(bytes, _)| bytes)
    }
    async fn read_prefix_measured<const PROFILE: bool>(&mut self, uri: &str, length: u64) -> io::Result<(Vec<u8>, ResourceReadTiming)> {
        let mut reader = ZipArchiveReader::new_with_inner(&mut self.source.reader, self.metadata.inner.clone());
        let index = self.metadata.index(uri)?;
        let (entry, open) = measure::<PROFILE, _>(reader.file(index)).await;
        let mut entry = entry.map_err(io::Error::other)?.take(length);
        let started = PROFILE.then(Instant::now);
        let mut bytes = Vec::with_capacity(length.min(256 * 1024) as usize);
        let allocate = started
            .map(|started| {
                let elapsed = started.elapsed();
                PreparationTiming { elapsed, active: elapsed, polls: 1 }
            })
            .unwrap_or_default();
        let (result, decode) = measure::<PROFILE, _>(entry.read_to_end(&mut bytes)).await;
        result?;
        let timing = if PROFILE {
            ResourceReadTiming { open, allocate, decode, compressed_bytes: self.metadata.inner.cdrs()[index].compressed_size().map_err(io::Error::other)?, output_bytes: bytes.len(), output_capacity: bytes.capacity() }
        } else {
            ResourceReadTiming::default()
        };
        Ok((bytes, timing))
    }
    pub(crate) async fn into_archive(self) -> io::Result<Archive> {
        Ok(Archive { metadata: self.metadata, factory: self.source.into_factory().await? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_zip::error::ZipError;
    use futures_lite::{future::block_on, io::Cursor as AsyncCursor};
    use std::io::{Cursor, Write};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    const CHAPTER: &[u8] = b"<html><body>Compatibility chapter</body></html>";

    fn archive_bytes(local_flags: u16, central_flags: u16, trailing: &[u8]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("chapter.xhtml", SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
        writer.write_all(CHAPTER).unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let end = bytes.len() - 22;
        let directory = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        bytes[6..8].copy_from_slice(&local_flags.to_le_bytes());
        bytes[directory + 8..directory + 10].copy_from_slice(&central_flags.to_le_bytes());
        bytes.extend_from_slice(trailing);
        bytes
    }

    fn read_chapter(bytes: Vec<u8>) -> io::Result<Vec<u8>> {
        block_on(async {
            let mut reader = ArchiveReader::open(Source::from_reader(Cursor::new(bytes))).await?;
            reader.read("chapter.xhtml", 1024).await
        })
    }

    #[test]
    fn trailing_padding_and_utf8_flag_differences_do_not_block_readable_entries() {
        for trailing in [b"".as_slice(), b"\0", b"\0\0", b"trailing data"] {
            for (local, central) in [(0, 0), (0, 0x800), (0x800, 0)] {
                assert_eq!(read_chapter(archive_bytes(local, central, trailing)).unwrap(), CHAPTER);
            }
        }
    }

    #[test]
    fn utf8_flag_differences_also_accept_matching_non_ascii_filename_bytes() {
        let name = "kapitel-å.xhtml";
        for (local, central) in [(0_u16, 0x800_u16), (0x800, 0)] {
            let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
            writer.start_file(name, SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
            writer.write_all(CHAPTER).unwrap();
            let mut bytes = writer.finish().unwrap().into_inner();
            let end = bytes.len() - 22;
            let directory = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
            bytes[6..8].copy_from_slice(&local.to_le_bytes());
            bytes[directory + 8..directory + 10].copy_from_slice(&central.to_le_bytes());
            block_on(async {
                let mut reader = ArchiveReader::open(Source::from_reader(Cursor::new(bytes))).await.unwrap();
                assert_eq!(reader.read(name, 1024).await.unwrap(), CHAPTER);
            });
        }
    }

    #[test]
    fn stale_local_flags_do_not_override_the_central_directory() {
        for bit in 0..16 {
            assert_eq!(read_chapter(archive_bytes(1 << bit, 0, b"\0")).unwrap(), CHAPTER, "flag bit {bit}");
        }
    }

    #[test]
    fn readable_content_survives_stale_checksums_and_local_filenames() {
        let mut corrupted = archive_bytes(0, 0x800, b"\0");
        let start = corrupted.windows(CHAPTER.len()).position(|bytes| bytes == CHAPTER).unwrap();
        corrupted[start] ^= 1;
        let mut expected = CHAPTER.to_vec();
        expected[0] ^= 1;
        assert_eq!(read_chapter(corrupted).unwrap(), expected);

        let mut renamed = archive_bytes(0, 0x800, b"\0");
        renamed[30] ^= 1;
        assert_eq!(read_chapter(renamed).unwrap(), CHAPTER);
    }

    #[test]
    fn compatibility_preserves_resource_limits_and_rejects_missing_directories() {
        block_on(async {
            let source = Source::from_reader(Cursor::new(archive_bytes(0, 0x800, b"\0")));
            let mut reader = ArchiveReader::open_with_limit(source, CHAPTER.len() as u64 - 1).await.unwrap();
            assert!(reader.read("chapter.xhtml", 1024).await.is_err());
        });
        let mut truncated = archive_bytes(0, 0x800, b"");
        truncated.truncate(truncated.len() - 22);
        assert!(read_chapter(truncated).is_err());
    }

    #[test]
    fn zip_defaults_keep_strict_archive_and_flag_validation() {
        block_on(async {
            let error = ZipArchiveReader::open(AsyncCursor::new(archive_bytes(0, 0, b"\0"))).await.err().unwrap();
            assert!(matches!(error, ZipError::EORIsNotEOA));
            let mut reader = ZipArchiveReader::open(AsyncCursor::new(archive_bytes(0, 0x800, b""))).await.unwrap();
            assert!(matches!(reader.file(0).await.err().unwrap(), ZipError::GPFHeaderMismatch));
        });
    }
}
