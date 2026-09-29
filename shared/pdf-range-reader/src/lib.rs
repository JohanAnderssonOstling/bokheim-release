//! Materialize only the PDF objects needed for bibliographic inspection. Large
//! image and attachment streams are never copied into the metadata document.
mod dependencies;
pub use dependencies::dependency_index;

pub type MetadataResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
use lopdf::xref::{Xref, XrefEntry, XrefType};
use lopdf::{Dictionary, Document, Object, ObjectId, ObjectStream, Reader, Stream};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Read, Seek, SeekFrom},
};

const MAX_OBJECT: usize = 16 * 1024 * 1024;
const MAX_RETAINED: usize = 64 * 1024 * 1024;
const MAX_OBJECTS: usize = 100_000;
const OFFSET_RECOVERY_RADIUS: u64 = 65536;
const MAX_OFFSET_SEARCHES: usize = 64;

pub fn load(reader: impl Read + Seek, pages: usize) -> MetadataResult<Document> {
    load_windows(reader, pages, false)
}

pub fn load_edges(reader: impl Read + Seek, pages: usize) -> MetadataResult<Document> {
    load_windows(reader, pages, true)
}

fn load_windows(reader: impl Read + Seek, pages: usize, both_ends: bool) -> MetadataResult<Document> {
    let mut source = open(reader)?;
    if let Ok(info) = source.document.trailer.get(b"Info").cloned() {
        // Bibliographic Info is optional. A malformed producer dictionary
        // must not prevent importing otherwise readable pages.
        let _ = source.graph(info, 0);
    }
    let root = source.document.trailer.get(b"Root")?.as_reference()?;
    let root_dict = source.object(root)?.as_dict()?.clone();
    if let Ok(metadata) = root_dict.get(b"Metadata").cloned() {
        source.graph(metadata, 0)?;
    }
    let page_root = root_dict.get(b"Pages")?.as_reference()?;
    let mut remaining = pages;
    let mut number = 0;
    source.pages(page_root, &mut remaining, &mut HashSet::new(), 0, false, &mut number)?;
    if both_ends {
        let mut remaining = pages;
        let count = source.object(page_root)?.as_dict()?.get(b"Count")?.as_i64()?;
        let mut number = u32::try_from(count)?.checked_add(1).ok_or("PDF page count overflow")?;
        source.pages(page_root, &mut remaining, &mut HashSet::new(), 0, true, &mut number)?;
    }
    Ok(source.document)
}

/// Materialize first-page resources for conservative embedded-cover extraction.
/// Existing object and retained-byte limits also bound image streams.
pub fn cover(mut reader: impl Read + Seek) -> MetadataResult<Document> {
    match cover_indexed(&mut reader) {
        Ok(document) => Ok(document),
        Err(original) => cover_linearized(&mut reader).map_err(|_| original),
    }
}

fn cover_indexed(reader: impl Read + Seek) -> MetadataResult<Document> {
    let mut source = open(reader)?;
    source.include_images = true;
    let root = source.document.trailer.get(b"Root")?.as_reference()?;
    let page_root = source.object(root)?.as_dict()?.get(b"Pages")?.as_reference()?;
    source.pages(page_root, &mut 1, &mut HashSet::new(), 0, false, &mut 0)?;
    let page = source
        .document
        .objects
        .values()
        .find_map(|object| {
            let page = object.as_dict().ok()?;
            (page.get(b"BokheimInspectionPage").and_then(Object::as_i64).ok() == Some(1)).then(|| page.clone())
        })
        .ok_or("PDF has no first page")?;
    source.cover_aux(&page)?;
    Ok(source.document)
}

// A damaged final xref must not force downloading/scanning an entire book.
// Linearized PDFs carry an independent first-page xref and a first-page object
// number in their header. Use those only when all inherited page properties are
// already present on that first page; never guess missing objects.
fn cover_linearized(reader: impl Read + Seek) -> MetadataResult<Document> {
    let mut source = Source {
        reader,
        length: 0,
        base: 0,
        document: Document::new(),
        loading: HashSet::new(),
        walked: HashSet::new(),
        retained: 0,
        spans: BTreeMap::new(),
        reads: Vec::new(),
        index_only: false,
        include_images: true,
        offset_hint: None,
        offset_searches: 0,
    };
    source.length = source.reader.seek(SeekFrom::End(0))?;
    let header = source.range(0, 65536)?;
    if !header.starts_with(b"%PDF-") {
        return Err("not a linearized PDF".into());
    }
    let obj = header.windows(3).position(|w| w == b"obj").ok_or("missing linearization object")?;
    let start = header[..obj].iter().rposition(|b| matches!(b, b'\n' | b'\r')).map_or(0, |i| i + 1);
    let mut words = header[start..obj].split(|b| b.is_ascii_whitespace()).filter(|w| !w.is_empty());
    let id = (u32::try_from(number(words.next())?)?, u16::try_from(number(words.next())?)?);
    let linearized = parse_object(&header[start..], id)?;
    let linearized = linearized.as_dict()?;
    if !linearized.has(b"Linearized") || u64::try_from(linearized.get(b"L")?.as_i64()?)? != source.length {
        return Err("invalid linearization header".into());
    }
    let page_id = (u32::try_from(linearized.get(b"O")?.as_i64()?)?, 0);
    let offset = xref_tables(&header).next().ok_or("missing first-page xref")?;
    let (xref, trailer) = table(&header[offset..])?;
    if trailer.has(b"Encrypt") {
        return Err("encrypted first-page recovery unsupported".into());
    }
    source.document.reference_table = xref;
    source.document.trailer = trailer;
    let page = source.object(page_id)?.as_dict()?.clone();
    if page.get(b"Type")?.as_name()? != b"Page" || !page.has(b"MediaBox") || !page.has(b"CropBox") || !page.has(b"Resources") || !page.has(b"Rotate") {
        return Err("first-page recovery requires self-contained page properties".into());
    }
    source.pages(page_id, &mut 1, &mut HashSet::new(), 0, false, &mut 0)?;
    source.cover_aux(&page)?;
    Ok(source.document)
}

fn xref_tables(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    bytes.windows(5).enumerate().filter_map(|(index, w)| (w.starts_with(b"xref") && w[4].is_ascii_whitespace() && (index == 0 || bytes[index - 1].is_ascii_whitespace())).then_some(index))
}

/// Load only the trailer and document root. Book identity must not require
/// decoding page content, fonts, images, or attachments.
pub fn document_root(reader: impl Read + Seek) -> MetadataResult<Document> {
    let mut source = open(reader)?;
    let root = source.document.trailer.get(b"Root")?.as_reference()?;
    source.object(root)?;
    Ok(source.document)
}

fn open<R: Read + Seek>(reader: R) -> MetadataResult<Source<R>> {
    let mut source = Source {
        reader,
        length: 0,
        base: 0,
        document: Document::new(),
        loading: HashSet::new(),
        walked: HashSet::new(),
        retained: 0,
        spans: BTreeMap::new(),
        reads: Vec::new(),
        index_only: false,
        include_images: false,
        offset_hint: None,
        offset_searches: 0,
    };
    source.length = source.reader.seek(SeekFrom::End(0))?;
    let header = source.range(0, 1024)?;
    source.base = header.windows(5).position(|bytes| bytes == b"%PDF-").ok_or("invalid PDF header")? as u64;
    source.length -= source.base;
    let tail = source.range(source.length.saturating_sub(65536), 65536)?;
    let start = tail.windows(9).rposition(|bytes| bytes == b"startxref").ok_or("PDF has no cross-reference pointer")?;
    let mut offset = std::str::from_utf8(tail[start + 9..].split(|byte| !byte.is_ascii_whitespace() && !byte.is_ascii_digit()).next().unwrap_or_default())?.trim().parse::<u64>()?;
    if let Err(original) = source.references(offset) {
        // Some producers leave a stale startxref after changing bytes. Search
        // only a bounded neighbourhood, and require a parseable xref table.
        let begin = offset.saturating_sub(65536).min(source.length);
        let bytes = source.range(begin, 131072)?;
        let mut repaired = false;
        for index in xref_tables(&bytes) {
            let candidate = begin + index as u64;
            if candidate == offset {
                continue;
            }
            source.document = Document::new();
            source.retained = 0;
            if source.references(candidate).is_ok() {
                offset = candidate;
                repaired = true;
                break;
            }
        }
        if !repaired {
            return Err(original);
        }
    }
    source.document.xref_start = offset as usize;
    source.document.max_id = source.document.reference_table.entries.keys().copied().max().unwrap_or(0);
    if let Ok(size) = source.document.trailer.get(b"Size").and_then(Object::as_i64) {
        source.document.max_id = source.document.max_id.max(u32::try_from(size.saturating_sub(1))?);
    }
    if let Ok(encryption) = source.document.trailer.get(b"Encrypt").cloned() {
        source.graph(encryption, 0)?;
        source.document.authenticate_raw_password(b"").map_err(|_| "PDF requires a non-empty password or uses unsupported encryption")?;
        source.document.encryption_state = Some(lopdf::EncryptionState::decode(&source.document, b"")?);
    }
    Ok(source)
}

struct Source<R> {
    reader: R,
    offset_hint: Option<i64>,
    offset_searches: usize,
    spans: BTreeMap<ObjectId, (u64, u64)>,
    reads: Vec<(u64, u64)>,
    index_only: bool,
    include_images: bool,
    length: u64,
    base: u64,
    document: Document,
    loading: HashSet<ObjectId>,
    retained: usize,
    walked: HashSet<ObjectId>,
}
impl<R: Read + Seek> Source<R> {
    fn cover_aux(&mut self, page: &Dictionary) -> MetadataResult<()> {
        if let Ok(group) = page.get(b"Group") {
            self.graph(group.clone(), 0)?;
        }
        // Cover extraction deliberately ignores annotations, including their resources.
        Ok(())
    }
    fn range(&mut self, offset: u64, count: usize) -> MetadataResult<Vec<u8>> {
        if offset > self.length || count > MAX_OBJECT {
            return Err("PDF range exceeds inspection limit".into());
        }
        let count = (count as u64).min(self.length - offset) as usize;
        self.reads.push((self.base + offset, count as u64));
        self.reader.seek(SeekFrom::Start(self.base + offset))?;
        let mut bytes = vec![0; count];
        self.reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn remember(&mut self, id: ObjectId, object: Object, cost: usize) -> MetadataResult<Object> {
        self.retained = self.retained.checked_add(cost.max(object_cost(&object))).ok_or("PDF inspection size overflow")?;
        if self.retained > MAX_RETAINED || self.document.objects.len() >= MAX_OBJECTS {
            return Err("PDF metadata exceeds inspection limit".into());
        }
        self.document.objects.insert(id, object.clone());
        Ok(object)
    }
    fn object(&mut self, id: ObjectId) -> MetadataResult<Object> {
        if let Some(object) = self.document.objects.get(&id) {
            return Ok(object.clone());
        }
        if self.loading.len() >= 100 || !self.loading.insert(id) {
            return Err("cyclic PDF object reference".into());
        }
        let result = self.object_inner(id).map_err(|error| format!("PDF object {} {}: {error}", id.0, id.1).into());
        self.loading.remove(&id);
        result
    }
    fn object_inner(&mut self, id: ObjectId) -> MetadataResult<Object> {
        match self.document.reference_table.get(id.0).cloned().ok_or("missing PDF cross-reference entry")? {
            XrefEntry::Normal { offset, generation } if generation == id.1 => {
                let (mut object, cost) = self.object_at(offset.into(), id)?;
                if let Some(state) = &self.document.encryption_state {
                    // Omitted image/attachment payloads are intentionally empty;
                    // do not feed them into a cipher expecting an IV and padding.
                    if object.as_stream().is_ok_and(|stream| stream.content.is_empty()) {
                        let stream = object.as_stream_mut()?;
                        let mut dictionary = Object::Dictionary(stream.dict.clone());
                        lopdf::encryption::decrypt_object(state, id, &mut dictionary)?;
                        stream.dict = dictionary.as_dict()?.clone();
                    } else {
                        lopdf::encryption::decrypt_object(state, id, &mut object)?;
                    }
                }
                self.remember(id, object, cost)
            }
            XrefEntry::Compressed { container, .. } => {
                let mut stream = self.object((container, 0))?.as_stream()?.clone();
                let objects = ObjectStream::new_with_limit(&mut stream, Some(MAX_OBJECT))?;
                for (object_id, object) in objects.objects {
                    self.remember(object_id, object, 256)?;
                }
                self.document.objects.get(&id).cloned().ok_or_else(|| "PDF object stream omitted requested object".into())
            }
            _ => Err("invalid PDF cross-reference entry".into()),
        }
    }
    fn header_at(&mut self, offset: u64, id: ObjectId) -> MetadataResult<bool> {
        if offset >= self.length {
            return Ok(false);
        }
        Ok(object_header(&self.range(offset, 128)?, id))
    }

    fn object_at(&mut self, offset: u64, id: ObjectId) -> MetadataResult<(Object, usize)> {
        // A correct recorded header always wins over a recovery hint, including
        // when different incremental revisions have different displacements.
        if self.header_at(offset, id)? {
            return self.at(offset, id);
        }
        if let Some(candidate) = self.offset_hint.and_then(|hint| offset.checked_add_signed(hint)) {
            if self.header_at(candidate, id)? {
                if let Ok(object) = self.at(candidate, id) {
                    return Ok(object);
                }
            }
        }
        if self.offset_searches >= MAX_OFFSET_SEARCHES {
            return Err("PDF object offset recovery search limit exceeded".into());
        }
        self.offset_searches += 1;
        let begin = offset.saturating_sub(OFFSET_RECOVERY_RADIUS).min(self.length);
        let bytes = self.range(begin, (OFFSET_RECOVERY_RADIUS * 2 + 128) as usize)?;
        let candidates = bytes
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| {
                let boundary = index == 0 || bytes[index - 1].is_ascii_whitespace();
                (boundary && byte.is_ascii_digit() && object_header(&bytes[index..], id)).then_some(begin + index as u64)
            })
            .filter(|candidate| candidate.abs_diff(offset) <= OFFSET_RECOVERY_RADIUS)
            .collect::<Vec<_>>();
        // Do not guess between duplicate object definitions in the search window.
        if candidates.len() != 1 {
            return Err("invalid PDF object offset: no unique matching header within recovery window".into());
        }
        let candidate = candidates[0];
        let object = self.at(candidate, id)?;
        self.offset_hint = Some(candidate as i64 - offset as i64);
        Ok(object)
    }

    fn at(&mut self, offset: u64, id: ObjectId) -> MetadataResult<(Object, usize)> {
        let mut count = 4096;
        loop {
            let bytes = self.range(offset, count)?;
            if let Ok(object) = parse_object(&bytes, id) {
                let dictionary = match &object {
                    Object::Dictionary(dict) => Some(dict),
                    Object::Stream(stream) => Some(&stream.dict),
                    _ => None,
                };
                if let Some(dictionary) = dictionary {
                    if let Some(start) = stream_start(&bytes) {
                        let mut stream = Stream::new(dictionary.clone(), Vec::new());
                        // Text extraction does not consume pixels, embedded files,
                        // or font programs. Their dictionaries remain available.
                        let image = dictionary.get(b"Subtype").and_then(Object::as_name).is_ok_and(|name| name == b"Image");
                        let attachment = dictionary.get(b"Type").and_then(Object::as_name).is_ok_and(|name| name == b"EmbeddedFile");
                        let length = match dictionary.get(b"Length")? {
                            Object::Reference(length) => self.object(*length)?.as_i64()?,
                            length => length.as_i64()?,
                        };
                        let length = usize::try_from(length)?;
                        let span = (start as u64).checked_add(length as u64).and_then(|n| n.checked_add(32)).ok_or("PDF span overflow")?.min(self.length - offset);
                        self.spans.insert(id, (self.base + offset, span));
                        let object_stream = dictionary.get(b"Type").and_then(Object::as_name).is_ok_and(|n| n == b"ObjStm" || n == b"XRef");
                        if (!image || self.include_images) && !attachment && (!self.index_only || object_stream) {
                            if length > MAX_OBJECT {
                                return Err("PDF metadata stream exceeds inspection limit".into());
                            }
                            stream.set_content(self.range(offset + start as u64, length)?);
                        }
                        let cost = start + stream.content.len();
                        return Ok((Object::Stream(stream), cost));
                    }
                    // A dictionary may end at the read boundary, just before its
                    // stream keyword. Read enough lookahead before deciding.
                    if dictionary_end(&bytes).is_some_and(|end| bytes.len() < end + 16) && offset + (bytes.len() as u64) < self.length {
                        if count >= MAX_OBJECT {
                            return Err("PDF dictionary exceeds inspection limit".into());
                        }
                        count = (count * 2).min(MAX_OBJECT);
                        continue;
                    }
                }
                self.spans.insert(id, (self.base + offset, bytes.len() as u64));
                return Ok((object, bytes.len()));
            }
            if bytes.len() < count || count >= MAX_OBJECT {
                return Err(if count >= MAX_OBJECT { "PDF object could not be parsed within the 16 MiB inspection limit" } else { "invalid or truncated PDF object at the matching header" }.into());
            }
            count = (count * 2).min(MAX_OBJECT);
        }
    }
    fn references(&mut self, offset: u64) -> MetadataResult<()> {
        let mut pending = vec![offset];
        let mut seen = HashSet::new();
        while let Some(offset) = pending.pop() {
            if !seen.insert(offset) {
                continue;
            }
            if seen.len() > 256 {
                return Err("PDF has too many incremental revisions".into());
            }
            let mut count = 4096;
            let (xref, trailer) = loop {
                let bytes = self.range(offset, count)?;
                if bytes.starts_with(b"xref") {
                    if let Ok(parsed) = table(&bytes) {
                        break parsed;
                    }
                } else {
                    let mut words = bytes.split(|byte| byte.is_ascii_whitespace()).filter(|word| !word.is_empty());
                    let id = (number(words.next())? as u32, u16::try_from(number(words.next())?)?);
                    let (object, _) = self.at(offset, id)?;
                    break stream_references(object.as_stream()?)?;
                }
                if bytes.len() < count || count >= MAX_OBJECT {
                    return Err("invalid or oversized PDF cross-reference table".into());
                }
                count = (count * 2).min(MAX_OBJECT);
            };
            if self.document.trailer.is_empty() {
                self.document.trailer = trailer.clone();
            }
            for (key, value) in trailer.iter() {
                if !self.document.trailer.has(key) {
                    self.document.trailer.set(key.clone(), value.clone());
                }
            }
            self.document.reference_table.merge(xref);
            if self.document.reference_table.entries.len() > MAX_OBJECTS {
                return Err("PDF cross-reference table exceeds inspection limit".into());
            }
            if let Ok(previous) = trailer.get(b"Prev").and_then(Object::as_i64) {
                pending.push(u64::try_from(previous)?);
            }
            // Process the supplementary stream before older revisions.
            if let Ok(stream) = trailer.get(b"XRefStm").and_then(Object::as_i64) {
                pending.push(u64::try_from(stream)?);
            }
        }
        Ok(())
    }
    fn pages(&mut self, id: ObjectId, remaining: &mut usize, seen: &mut HashSet<ObjectId>, depth: usize, reverse: bool, number: &mut u32) -> MetadataResult<()> {
        if *remaining == 0 {
            return Ok(());
        }
        if depth >= 256 || seen.len() >= MAX_OBJECTS || !seen.insert(id) {
            return Err("cyclic PDF page tree".into());
        }
        let dictionary = self.object(id)?.as_dict()?.clone();
        if let Ok(resources) = dictionary.get(b"Resources").cloned() {
            self.graph(resources, 0)?;
        }
        if dictionary.get(b"Type").and_then(Object::as_name).is_ok_and(|name| name == b"Page") {
            *number = if reverse { number.checked_sub(1) } else { number.checked_add(1) }.ok_or("invalid PDF page count")?;
            // Preserve physical page provenance even though middle page streams
            // and dictionaries are intentionally absent from this sparse document.
            if let Some(Object::Dictionary(page)) = self.document.objects.get_mut(&id) {
                page.set("BokheimInspectionPage", i64::from(*number));
            }
            if let Ok(contents) = dictionary.get(b"Contents").cloned() {
                self.graph(contents, 0)?;
            }
            *remaining -= 1;
        } else {
            let kids = dictionary.get(b"Kids")?.clone();
            let kids = match kids {
                Object::Reference(id) => self.object(id)?,
                kids => kids,
            };
            let mut children = kids.as_array()?.clone();
            if reverse {
                children.reverse();
            }
            for child in children {
                if *remaining == 0 {
                    break;
                }
                self.pages(child.as_reference()?, remaining, seen, depth + 1, reverse, number)?;
            }
        }
        Ok(())
    }
    fn graph(&mut self, object: Object, depth: usize) -> MetadataResult<()> {
        if depth > 100 {
            return Err("PDF metadata graph exceeds inspection depth".into());
        }
        match object {
            Object::Reference(id) => {
                if !self.walked.insert(id) {
                    return Ok(());
                }
                let object = self.object(id)?;
                self.graph(object, depth + 1)?;
            }
            Object::Array(items) => {
                for item in items {
                    self.graph(item, depth + 1)?;
                }
            }
            Object::Dictionary(dictionary) => self.dictionary_graph(dictionary, depth)?,
            Object::Stream(stream) => self.dictionary_graph(stream.dict, depth)?,
            _ => (),
        }
        Ok(())
    }
    fn dictionary_graph(&mut self, dictionary: Dictionary, depth: usize) -> MetadataResult<()> {
        for (key, value) in dictionary.iter() {
            if [b"Parent".as_slice(), b"FontFile", b"FontFile3", b"Metadata"].contains(&key.as_slice()) {
                continue;
            }
            if !self.include_images && [b"SMask".as_slice(), b"Mask", b"FontFile2"].contains(&key.as_slice()) {
                continue;
            }
            self.graph(value.clone(), depth + 1)?;
        }
        Ok(())
    }
}

fn object_cost(object: &Object) -> usize {
    fn dictionary_cost(dictionary: &Dictionary) -> usize {
        dictionary.iter().fold(64_usize, |total, (key, value)| total.saturating_add(key.len()).saturating_add(64).saturating_add(object_cost(value)))
    }
    match object {
        Object::Name(bytes) | Object::String(bytes, _) => bytes.len().saturating_add(32),
        Object::Array(items) => items.iter().fold(32_usize, |total, item| total.saturating_add(object_cost(item))),
        Object::Dictionary(dictionary) => dictionary_cost(dictionary),
        Object::Stream(stream) => stream.content.len().saturating_add(dictionary_cost(&stream.dict)),
        _ => 32,
    }
}

/// Match both object number and generation, with PDF token boundaries.
fn object_header(bytes: &[u8], id: ObjectId) -> bool {
    let mut rest = bytes;
    for expected in [u64::from(id.0), u64::from(id.1)] {
        let end = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
        if end == 0 || number(Some(&rest[..end])).ok() != Some(expected) {
            return false;
        }
        rest = &rest[end..];
        let spaces = rest.iter().take_while(|byte| byte.is_ascii_whitespace()).count();
        if spaces == 0 {
            return false;
        }
        rest = &rest[spaces..];
    }
    rest.strip_prefix(b"obj").is_some_and(|tail| tail.first().is_some_and(|byte| byte.is_ascii_whitespace() || b"<[/()%".contains(byte)))
}

fn parse_object(bytes: &[u8], id: ObjectId) -> lopdf::Result<Object> {
    let mut document = Document::new();
    document.reference_table.insert(id.0, XrefEntry::Normal { offset: 0, generation: id.1 });
    Reader { buffer: bytes, document, encryption_state: None, raw_objects: BTreeMap::new(), password: None, strict: false, max_decompressed_size: Some(MAX_OBJECT) }.get_object(id, &mut HashSet::new())
}
fn number(word: Option<&[u8]>) -> MetadataResult<u64> {
    Ok(std::str::from_utf8(word.ok_or("missing PDF integer")?)?.parse()?)
}
fn table(bytes: &[u8]) -> MetadataResult<(Xref, Dictionary)> {
    let trailer = bytes.windows(7).position(|word| word == b"trailer").ok_or("incomplete PDF trailer")?;
    let mut words = bytes[4..trailer].split(|byte| byte.is_ascii_whitespace()).filter(|word| !word.is_empty());
    let mut xref = Xref::new(0, XrefType::CrossReferenceTable);
    while let Some(first) = words.next() {
        let first = u32::try_from(number(Some(first))?)?;
        let count = u32::try_from(number(words.next())?)?;
        if count as usize > MAX_OBJECTS {
            return Err("PDF cross-reference section exceeds inspection limit".into());
        }
        for index in 0..count {
            let offset = u32::try_from(number(words.next())?)?;
            let generation = number(words.next())?;
            let entry = match words.next() {
                // Invalid unused entries occur in otherwise readable PDFs. They
                // must remain unresolvable, rather than rejecting every page.
                Some(b"n") if offset == 0 && generation > u64::from(u16::MAX) => XrefEntry::Free,
                Some(b"n") => XrefEntry::Normal { offset, generation: u16::try_from(generation)? },
                Some(b"f") => XrefEntry::Free,
                _ => return Err("invalid PDF cross-reference flag".into()),
            };
            xref.insert(first.checked_add(index).ok_or("PDF object number overflow")?, entry);
        }
    }
    let mut dictionary = b"1 0 obj\n".to_vec();
    dictionary.extend_from_slice(&bytes[trailer + 7..]);
    Ok((xref, parse_object(&dictionary, (1, 0))?.as_dict()?.clone()))
}
fn stream_references(stream: &Stream) -> MetadataResult<(Xref, Dictionary)> {
    let widths = stream.dict.get(b"W")?.as_array()?;
    if widths.len() != 3 {
        return Err("invalid PDF cross-reference widths".into());
    }
    let widths: Vec<usize> = widths.iter().map(|width| -> MetadataResult<usize> { Ok(usize::try_from(width.as_i64()?)?) }).collect::<MetadataResult<_>>()?;
    if widths.iter().any(|width| *width > 8) || widths.iter().sum::<usize>() == 0 {
        return Err("invalid PDF cross-reference widths".into());
    }
    let size = stream.dict.get(b"Size")?.as_i64()?;
    let index = stream.dict.get(b"Index").and_then(Object::as_array).cloned().unwrap_or_else(|_| vec![Object::Integer(0), Object::Integer(size)]);
    if index.len() % 2 != 0 {
        return Err("invalid PDF cross-reference index".into());
    }
    let bytes = stream.decompressed_content_with_limit(MAX_OBJECT)?;
    let mut position = 0;
    let mut xref = Xref::new(0, XrefType::CrossReferenceStream);
    for pair in index.chunks_exact(2) {
        let first = u32::try_from(pair[0].as_i64()?)?;
        let count = u32::try_from(pair[1].as_i64()?)?;
        if count as usize > MAX_OBJECTS {
            return Err("PDF cross-reference section exceeds inspection limit".into());
        }
        for index in 0..count {
            let mut fields = [0_u64; 3];
            for (field, width) in fields.iter_mut().zip(&widths) {
                for byte in bytes.get(position..position + width).ok_or("truncated PDF cross-reference stream")? {
                    *field = (*field << 8) | u64::from(*byte);
                }
                position += width;
            }
            if widths[0] == 0 {
                fields[0] = 1;
            }
            let entry = match fields[0] {
                0 => XrefEntry::Free,
                1 => XrefEntry::Normal { offset: u32::try_from(fields[1])?, generation: u16::try_from(fields[2])? },
                2 => XrefEntry::Compressed { container: u32::try_from(fields[1])?, index: u16::try_from(fields[2])? },
                _ => continue,
            };
            xref.insert(first.checked_add(index).ok_or("PDF object number overflow")?, entry);
        }
    }
    Ok((xref, stream.dict.clone()))
}

fn dictionary_end(bytes: &[u8]) -> Option<usize> {
    let mut index = 0;
    let mut depth = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            b'(' => {
                index += 1;
                let mut nesting = 1;
                while index < bytes.len() && nesting != 0 {
                    match bytes[index] {
                        b'\\' => index += 1,
                        b'(' => nesting += 1,
                        b')' => nesting -= 1,
                        _ => (),
                    }
                    index += 1;
                }
            }
            b'<' if bytes.get(index + 1) == Some(&b'<') => {
                depth += 1;
                index += 2;
            }
            b'<' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'>' {
                    index += 1;
                }
                index += 1;
            }
            b'>' if bytes.get(index + 1) == Some(&b'>') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }
    None
}
fn stream_start(bytes: &[u8]) -> Option<usize> {
    let mut index = dictionary_end(bytes)?;
    loop {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b'%') {
            break;
        }
        while bytes.get(index).is_some_and(|byte| !matches!(byte, b'\n' | b'\r')) {
            index += 1;
        }
    }
    if bytes.get(index..index + 6)? != b"stream" {
        return None;
    }
    index += 6;
    while bytes.get(index).is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
        index += 1;
    }
    if bytes.get(index) == Some(&b'\r') {
        index += 1;
    }
    if bytes.get(index) == Some(&b'\n') {
        index += 1;
    }
    Some(index)
}

/// Read only Info/XMP and the page-tree root, never page streams or images.
pub fn metadata(reader: impl Read + Seek) -> MetadataResult<Document> {
    let mut source = open(reader)?;
    if let Ok(info) = source.document.trailer.get(b"Info").cloned() {
        let _ = source.graph(info, 0);
    }
    let root = source.document.trailer.get(b"Root")?.as_reference()?;
    let root_dict = source.object(root)?.as_dict()?.clone();
    if let Ok(metadata) = root_dict.get(b"Metadata").cloned() {
        let _ = source.graph(metadata, 0);
    }
    let pages = root_dict.get(b"Pages")?.as_reference()?;
    if source.object(pages)?.as_dict()?.get(b"Count")?.as_i64()? <= 0 {
        return Err("PDF book contains no pages".into());
    }
    Ok(source.document)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        document.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
        let info = document.add_object(lopdf::dictionary! { "Title" => Object::string_literal("Computer Architecture") });
        let root = document.add_object(lopdf::dictionary! { "Type" => "Catalog" });
        document.trailer.set("Root", root);
        document.trailer.set("Info", info);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn recovers_stale_xref_and_object_offsets_after_inserted_bytes() {
        let mut bytes = fixture();
        let header_end = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        bytes.splice(header_end..header_end, vec![b' '; 19119]);
        let mut source = open(std::io::Cursor::new(bytes)).unwrap();
        let root = source.document.trailer.get(b"Root").unwrap().as_reference().unwrap();
        assert_eq!(source.object(root).unwrap().as_dict().unwrap().get(b"Type").unwrap().as_name().unwrap(), b"Catalog");
        assert_eq!(source.offset_hint, Some(19119));
        let info = source.document.trailer.get(b"Info").unwrap().as_reference().unwrap();
        assert_eq!(source.object(info).unwrap().as_dict().unwrap().get(b"Title").unwrap().as_str().unwrap(), b"Computer Architecture");
        assert_eq!(source.offset_searches, 1);
    }

    #[test]
    fn validates_generation_and_original_offset_before_recovery_hint() {
        let mut source = open(std::io::Cursor::new(fixture())).unwrap();
        let root = source.document.trailer.get(b"Root").unwrap().as_reference().unwrap();
        let XrefEntry::Normal { offset, .. } = source.document.reference_table.get(root.0).unwrap() else { panic!() };
        let offset = u64::from(*offset);
        source.offset_hint = Some(500);
        assert!(source.object_at(offset, root).is_ok());
        assert_eq!(source.offset_searches, 0);
        assert!(source.object_at(offset + 20, root).is_ok());
        assert_eq!(source.offset_hint, Some(-20));
        assert!(source.object_at(offset, (root.0, 1)).unwrap_err().to_string().contains("no unique matching header"));
        assert!(!object_header(b"12 0 object", (12, 0)));
        assert!(!object_header(b"112 0 obj <<", (12, 0)));
    }

    #[test]
    fn offset_recovery_is_bounded_and_rejects_duplicate_headers() {
        let mut bytes = fixture();
        let root = open(std::io::Cursor::new(bytes.clone())).unwrap().document.trailer.get(b"Root").unwrap().as_reference().unwrap();
        bytes.extend_from_slice(format!("\n{} {} obj << /Type /Catalog >> endobj\n", root.0, root.1).as_bytes());
        let mut source = open(std::io::Cursor::new(bytes)).unwrap();
        assert!(source.object_at(0, root).unwrap_err().to_string().contains("no unique matching header"));
        source.offset_searches = MAX_OFFSET_SEARCHES;
        assert!(source.object_at(0, root).unwrap_err().to_string().contains("search limit"));
        source.offset_searches = 0;
        assert!(source.object_at(source.length + OFFSET_RECOVERY_RADIUS + 1, root).is_err());
    }

    #[test]
    fn unused_cross_reference_generation_does_not_reject_existing_books() {
        let (xref, _) = table(b"xref\n0 1\n0000000000 65536 f\ntrailer\n<</Size 1>>").unwrap();
        assert!(matches!(xref.get(0), Some(XrefEntry::Free)));
    }
}
