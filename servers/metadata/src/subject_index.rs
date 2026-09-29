use super::{
    canonical_isbn13, classification_similarity_keys, error, Classification, ClassificationScheme, ClassificationSource, Connection, IsbnClassificationResult, LookupStatus, MetadataError, SnapshotDescription, WorkClassificationMatch,
    LCC_SCHEME,
};
use memmap2::Mmap;
use rusqlite::OpenFlags;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"BKSUBIDX";
const VERSION: u32 = 4;
const HEADER_LEN: usize = 56;
const RECORD_LEN: usize = 20;

pub(crate) struct SubjectIndex {
    mmap: Mmap,
    record_count: usize,
    blobs_offset: usize,
    snapshot: SnapshotDescription,
}

pub fn build_subject_index(source: &Path, output: &Path) -> Result<(), MetadataError> {
    build_subject_index_limited(source, output, None)
}

pub fn build_subject_index_limited(source: &Path, output: &Path, record_limit: Option<usize>) -> Result<(), MetadataError> {
    if output.exists() {
        return Err(MetadataError(format!("subject index output already exists: {}", output.display())));
    }
    let connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(error)?;
    let (dump_date, imported_at_ms): (String, i64) = connection.query_row("SELECT dump_date,imported_at_ms FROM snapshot WHERE singleton=1", (), |row| Ok((row.get(0)?, row.get(1)?))).map_err(error)?;
    if dump_date.len() > 16 {
        return Err(MetadataError("snapshot dump date is too long for the subject index".to_owned()));
    }
    let imported_at_ms = u64::try_from(imported_at_ms).map_err(|_| MetadataError("metadata snapshot has an invalid import timestamp".to_owned()))?;
    if !crate::loc_dump::has_lcc_dump(&connection)? {
        connection.execute_batch("CREATE TEMP VIEW isbn_lcc AS SELECT 0 AS isbn13,'' AS notation WHERE 0").map_err(error)?;
    }
    let available_records = usize::try_from(connection.query_row("SELECT COUNT(*) FROM (SELECT isbn13 FROM edition_isbn UNION SELECT isbn13 FROM isbn_lcc)", (), |row| row.get::<_, i64>(0)).map_err(error)?).map_err(error)?;
    let record_count = record_limit.map_or(available_records, |limit| limit.min(available_records));
    let records_bytes = record_count.checked_mul(RECORD_LEN).ok_or_else(|| MetadataError("subject index is too large".to_owned()))?;
    let blobs_offset = HEADER_LEN.checked_add(records_bytes).ok_or_else(|| MetadataError("subject index is too large".to_owned()))?;
    prepare_effective_work_classifications(&connection)?;
    let temporary = temporary_path(output, "index");
    let record_path = temporary_path(output, "records");
    for stale in [&temporary, &record_path] {
        if stale.exists() {
            fs::remove_file(stale).map_err(error)?;
        }
    }
    let result = (|| {
        let mut file = OpenOptions::new().create_new(true).read(true).write(true).open(&temporary).map_err(error)?;
        let mut record_file = OpenOptions::new().create_new(true).read(true).write(true).open(&record_path).map_err(error)?;
        file.set_len(u64::try_from(blobs_offset).map_err(error)?).map_err(error)?;
        file.seek(SeekFrom::Start(u64::try_from(blobs_offset).map_err(error)?)).map_err(error)?;
        let mut written_records = 0usize;
        stream_results(&connection, record_limit, |isbn, matches| {
            let blob = encode_matches(&matches)?;
            let relative_offset = file.stream_position().map_err(error)? - u64::try_from(blobs_offset).map_err(error)?;
            record_file.write_all(&isbn.to_le_bytes()).map_err(error)?;
            record_file.write_all(&relative_offset.to_le_bytes()).map_err(error)?;
            record_file.write_all(&u32::try_from(blob.len()).map_err(|_| MetadataError("subject record is too large".to_owned()))?.to_le_bytes()).map_err(error)?;
            file.write_all(&blob).map_err(error)?;
            written_records += 1;
            Ok(())
        })?;
        if written_records != record_count {
            return Err(MetadataError("subject database changed while its index was being built".to_owned()));
        }
        file.seek(SeekFrom::Start(0)).map_err(error)?;
        file.write_all(&encode_header(&dump_date, imported_at_ms, record_count, blobs_offset)?).map_err(error)?;
        record_file.seek(SeekFrom::Start(0)).map_err(error)?;
        io::copy(&mut BufReader::new(record_file), &mut file).map_err(error)?;
        file.sync_all().map_err(error)?;
        drop(file);
        fs::rename(&temporary, output).map_err(error)?;
        if let Some(parent) = output.parent() {
            File::open(parent).and_then(|directory| directory.sync_all()).map_err(error)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    let _ = fs::remove_file(&record_path);
    result
}

fn prepare_effective_work_classifications(connection: &Connection) -> Result<(), MetadataError> {
    connection
        .execute_batch(
            "CREATE TEMP TABLE effective_work_classification(
             work_id INTEGER NOT NULL, scheme INTEGER NOT NULL, notation TEXT NOT NULL,
             PRIMARY KEY(work_id,scheme,notation)
         ) WITHOUT ROWID;
         INSERT OR IGNORE INTO effective_work_classification
         SELECT classification.work_id,classification.scheme,classification.notation
         FROM work_classification AS classification
         JOIN (SELECT work_id FROM work_classification WHERE scheme=2 GROUP BY work_id HAVING COUNT(*) BETWEEN 1 AND 12) AS eligible USING(work_id) WHERE classification.scheme=2;
         INSERT OR IGNORE INTO effective_work_classification
         SELECT classification.work_id,classification.scheme,classification.notation
         FROM edition_work_classification AS classification
         JOIN (SELECT work_id FROM work_classification WHERE scheme=2 GROUP BY work_id HAVING COUNT(*) BETWEEN 1 AND 12) AS eligible USING(work_id)
         WHERE classification.scheme=2 AND NOT EXISTS (SELECT 1 FROM work_classification AS direct
                           WHERE direct.work_id=classification.work_id AND direct.scheme=classification.scheme);",
        )
        .map_err(error)?;

    let mut statement = connection
        .prepare(
            "SELECT edition.work_id,edition_classification.edition_id,edition_classification.scheme,edition_classification.notation
         FROM edition_classification JOIN edition USING(edition_id)
         WHERE edition.work_id IS NOT NULL AND edition_classification.scheme=2
         ORDER BY edition.work_id,edition_classification.edition_id,edition_classification.scheme,edition_classification.notation",
        )
        .map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    let mut current_work = None;
    let mut support = BTreeMap::<(i64, String), BTreeSet<i64>>::new();
    let mut classified_editions = BTreeSet::new();
    while let Some(row) = rows.next().map_err(error)? {
        let work_id = row.get::<_, i64>(0).map_err(error)?;
        if current_work.is_some_and(|current| current != work_id) {
            insert_consensus(connection, current_work.expect("set"), &support, classified_editions.len())?;
            support.clear();
            classified_editions.clear();
        }
        current_work = Some(work_id);
        let edition_id = row.get::<_, i64>(1).map_err(error)?;
        let scheme_id = row.get::<_, i64>(2).map_err(error)?;
        let scheme = match scheme_id {
            LCC_SCHEME => ClassificationScheme::LibraryOfCongress,
            _ => continue,
        };
        let notation = row.get::<_, String>(3).map_err(error)?;
        if let Some(notation) = metadata_contract::classification_consensus_notation(scheme, &notation) {
            classified_editions.insert(edition_id);
            support.entry((scheme_id, notation)).or_default().insert(edition_id);
        }
    }
    if let Some(work_id) = current_work {
        insert_consensus(connection, work_id, &support, classified_editions.len())?;
    }
    Ok(())
}

fn insert_consensus(connection: &Connection, work_id: i64, support: &BTreeMap<(i64, String), BTreeSet<i64>>, classified_edition_count: usize) -> Result<(), MetadataError> {
    let mut insert = connection.prepare_cached("INSERT OR IGNORE INTO effective_work_classification(work_id,scheme,notation) VALUES(?1,?2,?3)").map_err(error)?;
    for ((scheme, notation), _) in support.iter().filter(|(_, editions)| classified_edition_count == 1 || editions.len() >= 2) {
        insert.execute((work_id, scheme, notation)).map_err(error)?;
    }
    Ok(())
}

fn stream_results(connection: &Connection, record_limit: Option<usize>, mut emit: impl FnMut(u64, Vec<WorkClassificationMatch>) -> Result<(), MetadataError>) -> Result<(), MetadataError> {
    // Per-ISBN maps below provide deterministic group and evidence ordering.
    // Additional SQL sort keys force a global temporary sort at full scale.
    if record_limit == Some(0) {
        return Ok(());
    }
    let ordering = " ORDER BY edition_isbn.isbn13";
    let mut base_statement = connection.prepare(&format!("SELECT edition_isbn.isbn13,edition.edition_id,edition.work_id,0,NULL,NULL FROM edition_isbn JOIN edition USING(edition_id){ordering}")).map_err(error)?;
    let mut exact_statement = connection.prepare(&format!("SELECT edition_isbn.isbn13,edition.edition_id,edition.work_id,1,classification.scheme,classification.notation FROM edition_isbn JOIN edition USING(edition_id) JOIN edition_classification AS classification USING(edition_id){ordering}")).map_err(error)?;
    let mut work_statement = connection.prepare(&format!("SELECT edition_isbn.isbn13,edition.edition_id,edition.work_id,2,classification.scheme,classification.notation FROM edition_isbn JOIN edition USING(edition_id) JOIN effective_work_classification AS classification USING(work_id){ordering}")).map_err(error)?;
    let mut lc_statement = connection.prepare("SELECT DISTINCT isbn13,0,NULL,3,2,notation FROM isbn_lcc ORDER BY isbn13").map_err(error)?;
    let mut streams = [base_statement.query([]).map_err(error)?, exact_statement.query([]).map_err(error)?, work_statement.query([]).map_err(error)?, lc_statement.query([]).map_err(error)?];
    let mut heads = [next_evidence(&mut streams[0])?, next_evidence(&mut streams[1])?, next_evidence(&mut streams[2])?, next_evidence(&mut streams[3])?];
    let mut current_isbn = None;
    let mut emitted = 0usize;
    let mut groups = BTreeMap::<(Option<i64>, i64), BuildMatch>::new();
    let mut lc_codes = BTreeSet::new();
    while let Some(stream_index) = heads.iter().enumerate().filter_map(|(index, row)| row.as_ref().map(|row| (index, row))).min_by_key(|(_, row)| (row.isbn, row.source)).map(|(index, _)| index) {
        let row = heads[stream_index].take().expect("selected stream has a row");
        heads[stream_index] = next_evidence(&mut streams[stream_index])?;
        let isbn = row.isbn;
        if current_isbn.is_some_and(|current| current != isbn) {
            emit_built(current_isbn.expect("set"), &mut groups, &mut lc_codes, &mut emit)?;
            emitted += 1;
            if record_limit.is_some_and(|limit| emitted >= limit) {
                return Ok(());
            }
        }
        current_isbn = Some(isbn);
        if row.source == 3 {
            lc_codes.insert(row.notation.expect("LC dump evidence has a notation"));
            continue;
        }
        let edition_id = row.edition_id;
        let work_id = row.work_id;
        let matched = groups.entry((work_id, work_id.unwrap_or(edition_id))).or_default();
        matched.edition_ids.insert(edition_id);
        let source_id = row.source;
        if source_id == 0 {
            continue;
        }
        let scheme = match row.scheme.expect("classification evidence has a scheme") {
            LCC_SCHEME => ClassificationScheme::LibraryOfCongress,
            _ => continue,
        };
        let source = if source_id == 1 { ClassificationSource::ExactEdition } else { ClassificationSource::Work };
        let notation = row.notation.expect("classification evidence has notation");
        if source == ClassificationSource::ExactEdition {
            if matched.exact_schemes.insert(scheme) {
                matched.classifications.retain(|(existing_scheme, _), _| *existing_scheme != scheme);
            }
        } else if matched.exact_schemes.contains(&scheme) {
            continue;
        }
        matched.classifications.entry((scheme, notation)).and_modify(|existing| *existing = (*existing).min(source)).or_insert(source);
    }
    if let Some(isbn) = current_isbn {
        emit_built(isbn, &mut groups, &mut lc_codes, &mut emit)?;
    }
    Ok(())
}

struct EvidenceRow {
    isbn: i64,
    edition_id: i64,
    work_id: Option<i64>,
    source: i64,
    scheme: Option<i64>,
    notation: Option<String>,
}

fn next_evidence(rows: &mut rusqlite::Rows<'_>) -> Result<Option<EvidenceRow>, MetadataError> {
    rows.next()
        .map_err(error)?
        .map(|row| {
            Ok(EvidenceRow {
                isbn: row.get(0).map_err(error)?,
                edition_id: row.get(1).map_err(error)?,
                work_id: row.get(2).map_err(error)?,
                source: row.get(3).map_err(error)?,
                scheme: row.get(4).map_err(error)?,
                notation: row.get(5).map_err(error)?,
            })
        })
        .transpose()
}

#[derive(Default)]
struct BuildMatch {
    edition_ids: BTreeSet<i64>,
    classifications: BTreeMap<(ClassificationScheme, String), ClassificationSource>,
    exact_schemes: BTreeSet<ClassificationScheme>,
}

fn emit_built(isbn: i64, groups: &mut BTreeMap<(Option<i64>, i64), BuildMatch>, lc_codes: &mut BTreeSet<String>, emit: &mut impl FnMut(u64, Vec<WorkClassificationMatch>) -> Result<(), MetadataError>) -> Result<(), MetadataError> {
    let mut matches = std::mem::take(groups)
        .into_iter()
        .map(|((work_id, _), matched)| {
            let classifications =
                metadata_contract::filter_classification_outliers(matched.classifications.into_iter().map(|((scheme, notation), source)| Classification { evidence: Vec::new(), scheme, notation, source }), classification_similarity_keys);
            WorkClassificationMatch { open_library_work_id: work_id.map(|id| format!("OL{id}W")), exact_edition_ids: matched.edition_ids.into_iter().map(super::openlibrary::authority_edition_key).collect(), classifications }
        })
        .collect();
    crate::loc_dump::merge_lcc(&mut matches, std::mem::take(lc_codes));
    emit(u64::try_from(isbn).map_err(error)?, matches)
}

impl SubjectIndex {
    pub(crate) fn open(path: &Path) -> Result<Self, MetadataError> {
        let file = File::open(path).map_err(error)?;
        // SAFETY: this is a read-only mapping and `Mmap` owns the mapping after `file` closes.
        let mmap = unsafe { Mmap::map(&file) }.map_err(error)?;
        if mmap.len() < HEADER_LEN || &mmap[..8] != MAGIC || read_u32(&mmap, 8)? != VERSION {
            return Err(MetadataError("invalid subject index header".to_owned()));
        }
        let date_len = usize::try_from(read_u32(&mmap, 12)?).map_err(error)?;
        if date_len > 16 {
            return Err(MetadataError("invalid subject index dump date".to_owned()));
        }
        let dump_date = std::str::from_utf8(&mmap[16..16 + date_len]).map_err(error)?.to_owned();
        let imported_at_ms = read_u64(&mmap, 32)?;
        let record_count = usize::try_from(read_u64(&mmap, 40)?).map_err(error)?;
        let blobs_offset = usize::try_from(read_u64(&mmap, 48)?).map_err(error)?;
        let expected_offset = HEADER_LEN.checked_add(record_count.checked_mul(RECORD_LEN).ok_or_else(|| MetadataError("invalid subject index size".to_owned()))?).ok_or_else(|| MetadataError("invalid subject index size".to_owned()))?;
        if blobs_offset != expected_offset || blobs_offset > mmap.len() {
            return Err(MetadataError("invalid subject index record table".to_owned()));
        }
        Ok(Self { mmap, record_count, blobs_offset, snapshot: SnapshotDescription { dump_date, imported_at_ms } })
    }

    pub(crate) fn validate_snapshot(&self, expected: &SnapshotDescription) -> Result<(), MetadataError> {
        if self.snapshot.dump_date != expected.dump_date || self.snapshot.imported_at_ms != expected.imported_at_ms {
            return Err(MetadataError(format!("subject index snapshot {} does not match metadata snapshot {}", self.snapshot.dump_date, expected.dump_date)));
        }
        Ok(())
    }

    pub(crate) fn lookup(&self, requested_isbn: String) -> Result<IsbnClassificationResult, MetadataError> {
        let Some(isbn) = canonical_isbn13(&requested_isbn) else {
            return Ok(IsbnClassificationResult { requested_isbn, canonical_isbn13: None, status: LookupStatus::InvalidIsbn, matches: Vec::new() });
        };
        let isbn_key = u64::try_from(isbn).map_err(error)?;
        let mut low = 0usize;
        let mut high = self.record_count;
        while low < high {
            let middle = low + (high - low) / 2;
            if self.record_key(middle)? < isbn_key {
                low = middle + 1
            } else {
                high = middle
            }
        }
        let canonical_isbn13 = Some(format!("{isbn:013}"));
        if low == self.record_count || self.record_key(low)? != isbn_key {
            return Ok(IsbnClassificationResult { requested_isbn, canonical_isbn13, status: LookupStatus::NoMatch, matches: Vec::new() });
        }
        let record = HEADER_LEN + low * RECORD_LEN;
        let offset = usize::try_from(read_u64(&self.mmap, record + 8)?).map_err(error)?;
        let length = usize::try_from(read_u32(&self.mmap, record + 16)?).map_err(error)?;
        let start = self.blobs_offset.checked_add(offset).ok_or_else(|| MetadataError("invalid subject index blob offset".to_owned()))?;
        let end = start.checked_add(length).ok_or_else(|| MetadataError("invalid subject index blob length".to_owned()))?;
        let matches = decode_matches(self.mmap.get(start..end).ok_or_else(|| MetadataError("truncated subject index blob".to_owned()))?)?;
        let status = match matches.len() {
            0 => LookupStatus::NoMatch,
            1 => LookupStatus::Matched,
            _ => LookupStatus::Ambiguous,
        };
        Ok(IsbnClassificationResult { requested_isbn, canonical_isbn13, status, matches })
    }

    fn record_key(&self, index: usize) -> Result<u64, MetadataError> {
        read_u64(&self.mmap, HEADER_LEN + index * RECORD_LEN)
    }
}

fn encode_header(dump_date: &str, imported_at_ms: u64, record_count: usize, blobs_offset: usize) -> Result<[u8; HEADER_LEN], MetadataError> {
    let mut header = [0_u8; HEADER_LEN];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&VERSION.to_le_bytes());
    header[12..16].copy_from_slice(&u32::try_from(dump_date.len()).map_err(error)?.to_le_bytes());
    header[16..16 + dump_date.len()].copy_from_slice(dump_date.as_bytes());
    header[32..40].copy_from_slice(&imported_at_ms.to_le_bytes());
    header[40..48].copy_from_slice(&u64::try_from(record_count).map_err(error)?.to_le_bytes());
    header[48..56].copy_from_slice(&u64::try_from(blobs_offset).map_err(error)?.to_le_bytes());
    Ok(header)
}

fn encode_matches(matches: &[WorkClassificationMatch]) -> Result<Vec<u8>, MetadataError> {
    let mut bytes = Vec::new();
    put_u16(&mut bytes, matches.len())?;
    for matched in matches {
        let work_id = matched.open_library_work_id.as_deref().and_then(|id| super::open_library_id(id, 'W')).unwrap_or(-1);
        bytes.extend_from_slice(&work_id.to_le_bytes());
        put_u16(&mut bytes, matched.exact_edition_ids.len())?;
        for id in &matched.exact_edition_ids {
            bytes.extend_from_slice(&super::openlibrary::authority_edition_id(id).ok_or_else(|| MetadataError("invalid edition identifier while building subject index".to_owned()))?.to_le_bytes());
        }
        let active = matched.classifications.iter().filter(|c| c.scheme != ClassificationScheme::DeweyDecimal).collect::<Vec<_>>();
        put_u16(&mut bytes, active.len())?;
        for classification in active {
            bytes.push(match classification.scheme {
                ClassificationScheme::LibraryOfCongress => 1,
                ClassificationScheme::DeweyDecimal => 2,
                ClassificationScheme::Bisac => 3,
            });
            bytes.push(match classification.source {
                ClassificationSource::ExactEdition => 1,
                ClassificationSource::Work => 2,
            });
            put_u16(&mut bytes, classification.notation.len())?;
            bytes.extend_from_slice(classification.notation.as_bytes());
        }
    }
    Ok(bytes)
}

fn decode_matches(bytes: &[u8]) -> Result<Vec<WorkClassificationMatch>, MetadataError> {
    let mut cursor = 0usize;
    let mut matches = Vec::with_capacity(take_u16(bytes, &mut cursor)?);
    for _ in 0..matches.capacity() {
        let work_id = take_i64(bytes, &mut cursor)?;
        let edition_count = take_u16(bytes, &mut cursor)?;
        let mut exact_edition_ids = Vec::with_capacity(edition_count);
        for _ in 0..edition_count {
            exact_edition_ids.push(super::openlibrary::authority_edition_key(take_i64(bytes, &mut cursor)?));
        }
        let classification_count = take_u16(bytes, &mut cursor)?;
        let mut classifications = Vec::with_capacity(classification_count);
        for _ in 0..classification_count {
            let scheme = match take_u8(bytes, &mut cursor)? {
                1 => ClassificationScheme::LibraryOfCongress,
                2 => ClassificationScheme::DeweyDecimal,
                3 => ClassificationScheme::Bisac,
                _ => return Err(MetadataError("invalid subject index classification scheme".to_owned())),
            };
            let source = match take_u8(bytes, &mut cursor)? {
                1 => ClassificationSource::ExactEdition,
                2 => ClassificationSource::Work,
                _ => return Err(MetadataError("invalid subject index classification source".to_owned())),
            };
            let length = take_u16(bytes, &mut cursor)?;
            let notation = std::str::from_utf8(take(bytes, &mut cursor, length)?).map_err(error)?.to_owned();
            if scheme != ClassificationScheme::DeweyDecimal {
                classifications.push(Classification { evidence: Vec::new(), scheme, source, notation });
            }
        }
        matches.push(WorkClassificationMatch { open_library_work_id: (work_id >= 0).then(|| format!("OL{work_id}W")), exact_edition_ids, classifications });
    }
    if cursor != bytes.len() {
        return Err(MetadataError("invalid trailing data in subject index blob".to_owned()));
    }
    Ok(matches)
}

fn temporary_path(output: &Path, kind: &str) -> PathBuf {
    let mut value = output.as_os_str().to_owned();
    value.push(format!(".tmp-{kind}-{}", std::process::id()));
    PathBuf::from(value)
}
fn put_u16(bytes: &mut Vec<u8>, value: usize) -> Result<(), MetadataError> {
    bytes.extend_from_slice(&u16::try_from(value).map_err(|_| MetadataError("subject index list is too long".to_owned()))?.to_le_bytes());
    Ok(())
}
fn read_u32(bytes: &[u8], at: usize) -> Result<u32, MetadataError> {
    Ok(u32::from_le_bytes(take_at(bytes, at, 4)?.try_into().expect("fixed size")))
}
fn read_u64(bytes: &[u8], at: usize) -> Result<u64, MetadataError> {
    Ok(u64::from_le_bytes(take_at(bytes, at, 8)?.try_into().expect("fixed size")))
}
fn take_at(bytes: &[u8], at: usize, length: usize) -> Result<&[u8], MetadataError> {
    bytes.get(at..at.checked_add(length).ok_or_else(|| MetadataError("invalid subject index offset".to_owned()))?).ok_or_else(|| MetadataError("truncated subject index".to_owned()))
}
fn take<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], MetadataError> {
    let value = take_at(bytes, *cursor, length)?;
    *cursor += length;
    Ok(value)
}
fn take_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8, MetadataError> {
    Ok(take(bytes, cursor, 1)?[0])
}
fn take_u16(bytes: &[u8], cursor: &mut usize) -> Result<usize, MetadataError> {
    Ok(usize::from(u16::from_le_bytes(take(bytes, cursor, 2)?.try_into().expect("fixed size"))))
}
fn take_i64(bytes: &[u8], cursor: &mut usize) -> Result<i64, MetadataError> {
    Ok(i64::from_le_bytes(take(bytes, cursor, 8)?.try_into().expect("fixed size")))
}

#[cfg(test)]
mod legacy_dewey_tests {
    #[test]
    fn old_index_dewey_entries_are_consumed_but_not_returned() {
        // One work, no edition IDs, followed by a legacy Dewey code and an LCC code.
        let bytes = [1, 0, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 2, 0, 2, 1, 3, 0, b'5', b'0', b'0', 1, 1, 4, 0, b'E', b'7', b'5', b'7'];
        let matches = super::decode_matches(&bytes).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].classifications.len(), 1);
        assert_eq!(matches[0].classifications[0].notation, "E757");
    }
}
