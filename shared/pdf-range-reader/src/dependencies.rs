//! Bounded page dependency index. Content programs select graphics resources;
//! image and font payloads are described by byte span without decoding them.
//! Object-stream containers, inherited resources, masks and annotation appearances
//! remain dependencies. Unsupported files use range fallback.
use super::*;
use pdf_view_common::{PdfByteRange, PdfDependencyIndex};

pub fn dependency_index(reader: impl Read + Seek) -> MetadataResult<PdfDependencyIndex> {
    let mut source = open(reader)?;
    source.index_only = true;
    let root = source.document.trailer.get(b"Root")?.as_reference()?;
    let root_dict = source.object(root)?.as_dict()?.clone();
    let page_root = root_dict.get(b"Pages")?.as_reference()?;
    let mut startup = source.reads.clone();
    let mut leaves = Vec::new();
    collect_pages(&mut source, page_root, Vec::new(), &mut HashSet::new(), &mut leaves)?;
    let mut pages = Vec::new();
    let mut traversed = 0usize;
    let mut span_count = 0usize;
    for (id, parents) in leaves {
        let mut seen = HashSet::new();
        seen.insert(id);
        let mut page = source.object(id)?.as_dict()?.clone();
        let mut resources = page.remove(b"Resources");
        // Resources inherit from the nearest ancestor, they do not merge.
        for parent in parents.into_iter().rev() {
            seen.insert(parent);
            if resources.is_none() {
                resources = source.object(parent)?.as_dict()?.get(b"Resources").ok().cloned();
            }
        }
        let used = resource_names(&mut source, page.get(b"Contents").ok().cloned())?;
        walk_dictionary(&mut source, page, &mut seen, 0)?;
        if let Some(resources) = resources {
            walk_resources(&mut source, resources, &used, &mut seen, 0)?;
        }
        traversed += seen.len();
        if traversed > 1_000_000 {
            return Err("PDF dependency traversal exceeds limits".into());
        }
        let spans = seen.into_iter().filter_map(|id| {
            let id = match source.document.reference_table.get(id.0) {
                Some(XrefEntry::Compressed { container, .. }) => (*container, 0),
                _ => id,
            };
            source.spans.get(&id).copied()
        });
        let spans = coalesce(spans);
        span_count += spans.len();
        if span_count > 100_000 {
            return Err("PDF dependency index exceeds limits".into());
        }
        pages.push(spans);
    }
    // Fonts are shared startup dependencies, including embedded font programs.
    let fonts = source.document.objects.iter().filter_map(|(&id, object)| object.as_dict().ok().filter(|dict| dict.get(b"Type").and_then(Object::as_name).is_ok_and(|name| name == b"Font")).map(|_| id)).collect::<Vec<_>>();
    let mut font_graph = HashSet::new();
    for id in fonts {
        walk(&mut source, Object::Reference(id), &mut font_graph, 0)?;
    }
    for id in font_graph {
        let id = match source.document.reference_table.get(id.0) {
            Some(XrefEntry::Compressed { container, .. }) => (*container, 0),
            _ => id,
        };
        if let Some(span) = source.spans.get(&id) {
            startup.push(*span);
        }
    }
    let index = PdfDependencyIndex { length: source.base + source.length, startup: coalesce(startup), pages };
    if !index.valid(index.pages.len()) {
        return Err("PDF dependency index exceeds limits".into());
    }
    Ok(index)
}

fn collect_pages<R: Read + Seek>(source: &mut Source<R>, id: ObjectId, mut parents: Vec<ObjectId>, seen: &mut HashSet<ObjectId>, leaves: &mut Vec<(ObjectId, Vec<ObjectId>)>) -> MetadataResult<()> {
    if parents.len() > 100 || !seen.insert(id) || seen.len() > MAX_OBJECTS {
        return Err("invalid PDF page tree".into());
    }
    let dictionary = source.object(id)?.as_dict()?.clone();
    if dictionary.get(b"Type").and_then(Object::as_name).is_ok_and(|v| v == b"Page") {
        leaves.push((id, parents));
    } else {
        parents.push(id);
        let kids = dictionary.get(b"Kids")?.clone();
        let kids = match kids {
            Object::Reference(id) => source.object(id)?,
            other => other,
        };
        for child in kids.as_array()? {
            collect_pages(source, child.as_reference()?, parents.clone(), seen, leaves)?;
        }
    }
    Ok(())
}

fn walk<R: Read + Seek>(source: &mut Source<R>, value: Object, seen: &mut HashSet<ObjectId>, depth: usize) -> MetadataResult<()> {
    if depth > 100 || seen.len() > MAX_OBJECTS {
        return Err("PDF dependency graph exceeds limits".into());
    }
    match value {
        Object::Reference(id) => {
            if seen.insert(id) {
                let value = source.object(id)?;
                if value.as_stream().is_ok_and(|stream| stream.dict.get(b"Subtype").and_then(Object::as_name).is_ok_and(|kind| kind == b"Form") || stream.dict.get(b"PatternType").and_then(Object::as_i64).is_ok_and(|kind| kind == 1)) {
                    let used = resource_names(source, Some(Object::Reference(id)))?;
                    let mut dict = value.as_stream()?.dict.clone();
                    let resources = dict.remove(b"Resources");
                    walk_dictionary(source, dict, seen, depth + 1)?;
                    if let Some(resources) = resources {
                        walk_resources(source, resources, &used, seen, depth + 1)?;
                    } else if !used.is_empty() {
                        return Err("PDF form uses inherited graphics resources".into());
                    }
                    return Ok(());
                }
                if depth > 0 && value.as_dict().is_ok_and(|dict| dict.get(b"Type").and_then(Object::as_name).is_ok_and(|kind| kind == b"Page" || kind == b"Pages")) {
                    return Ok(());
                }
                walk(source, value, seen, depth + 1)?;
            }
        }
        Object::Array(values) => {
            for value in values {
                walk(source, value, seen, depth + 1)?;
            }
        }
        Object::Dictionary(dict) => walk_dictionary(source, dict, seen, depth)?,
        Object::Stream(stream) => walk_dictionary(source, stream.dict, seen, depth)?,
        _ => (),
    }
    Ok(())
}
fn walk_dictionary<R: Read + Seek>(source: &mut Source<R>, dict: Dictionary, seen: &mut HashSet<ObjectId>, depth: usize) -> MetadataResult<()> {
    let kind = dict.get(b"Type").and_then(Object::as_name).unwrap_or_default();
    let page = kind == b"Page" || kind == b"Pages";
    let annotation = kind == b"Annot" || (dict.has(b"Rect") && dict.has(b"Subtype"));
    for (key, value) in dict.iter() {
        // Apply structural exclusions only in their owning dictionaries: a
        // resource named /A, /P or /D is still a real graphics dependency.
        if page && [b"Parent".as_slice(), b"Kids", b"AA", b"Metadata"].contains(&key.as_slice()) {
            continue;
        }
        if annotation && [b"P".as_slice(), b"Parent", b"Dest", b"A", b"AA", b"Popup", b"IRT"].contains(&key.as_slice()) {
            continue;
        }
        walk(source, value.clone(), seen, depth + 1)?;
    }
    Ok(())
}
/// Only named drawing resources are warmed. Shared /XObject dictionaries can
/// contain every image in the book even when one page paints just one of them.
fn resource_names<R: Read + Seek>(source: &mut Source<R>, contents: Option<Object>) -> MetadataResult<HashSet<(Vec<u8>, Vec<u8>)>> {
    let mut bytes = Vec::new();
    if let Some(contents) = contents {
        content_bytes(source, contents, &mut bytes, 0)?;
    }
    let content = lopdf::content::Content::decode(&bytes)?;
    let mut used = HashSet::new();
    for operation in content.operations {
        let category: &[u8] = match operation.operator.as_str() {
            "Do" => b"XObject",
            "gs" => b"ExtGState",
            "sh" => b"Shading",
            "SCN" | "scn" => b"Pattern",
            _ => continue,
        };
        for operand in operation.operands {
            if let Object::Name(name) = operand {
                used.insert((category.to_vec(), name));
            }
        }
    }
    Ok(used)
}
fn content_bytes<R: Read + Seek>(source: &mut Source<R>, object: Object, bytes: &mut Vec<u8>, depth: usize) -> MetadataResult<()> {
    if depth > 100 || bytes.len() >= MAX_OBJECT {
        return Err("PDF content indexing exceeds limits".into());
    }
    match object {
        Object::Reference(id) => {
            let object = source.object(id)?;
            let object = if object.as_stream().is_ok_and(|s| s.content.is_empty()) {
                // The graph reader omitted this stream's bytes. Decode only
                // content programs; never image payloads or font programs.
                source.document.objects.remove(&id);
                source.index_only = false;
                let result = source.object(id);
                source.index_only = true;
                result?
            } else {
                object
            };
            content_bytes(source, object, bytes, depth + 1)?;
        }
        Object::Array(objects) => {
            for object in objects {
                content_bytes(source, object, bytes, depth + 1)?;
            }
        }
        Object::Stream(stream) => {
            let decoded = stream.decompressed_content_with_limit(MAX_OBJECT - bytes.len())?;
            bytes.extend(decoded);
            bytes.push(b'\n');
        }
        Object::Null => (),
        _ => return Err("invalid PDF content stream".into()),
    }
    Ok(())
}
fn resource_dictionary<R: Read + Seek>(source: &mut Source<R>, value: Object, seen: &mut HashSet<ObjectId>) -> MetadataResult<Dictionary> {
    match value {
        Object::Reference(id) => {
            seen.insert(id);
            Ok(source.object(id)?.as_dict()?.clone())
        }
        Object::Dictionary(dict) => Ok(dict),
        _ => Err("invalid PDF resource dictionary".into()),
    }
}
fn walk_resources<R: Read + Seek>(source: &mut Source<R>, value: Object, used: &HashSet<(Vec<u8>, Vec<u8>)>, seen: &mut HashSet<ObjectId>, depth: usize) -> MetadataResult<()> {
    let resources = resource_dictionary(source, value, seen)?;
    for (category, value) in resources.iter() {
        if [b"XObject".as_slice(), b"ExtGState", b"Shading", b"Pattern"].contains(&category.as_slice()) {
            let entries = resource_dictionary(source, value.clone(), seen)?;
            for (name, value) in entries.iter() {
                if used.contains(&(category.clone(), name.clone())) {
                    walk(source, value.clone(), seen, depth + 1)?;
                }
            }
        } else {
            walk(source, value.clone(), seen, depth + 1)?;
        }
    }
    Ok(())
}

fn coalesce(spans: impl IntoIterator<Item = (u64, u64)>) -> Vec<PdfByteRange> {
    let mut spans = spans.into_iter().filter(|(_, length)| *length > 0).collect::<Vec<_>>();
    spans.sort_unstable();
    let mut result: Vec<PdfByteRange> = Vec::new();
    for (offset, length) in spans {
        if let Some(last) = result.last_mut() {
            if offset <= last.offset + last.length {
                last.length = (offset + length).max(last.offset + last.length) - last.offset;
                continue;
            }
        }
        result.push(PdfByteRange { offset, length });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;
    #[test]
    fn indexes_inherited_fonts_images_masks_and_compressed_object_containers() {
        for modern in [false, true] {
            let mut doc = Document::with_version("1.7");
            let root = doc.new_object_id();
            let font_program = doc.add_object(Stream::new(dictionary! {}, vec![17; 160_000]));
            let descriptor = doc.add_object(dictionary! { "Type" => "FontDescriptor", "FontFile" => font_program });
            let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Fixture", "FontDescriptor" => descriptor });
            let mask = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image" }, vec![33; 240_000]));
            let image = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "SMask" => mask }, vec![77; 240_000]));
            let unused = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image" }, vec![88; 240_000]));
            let content = doc.add_object(Stream::new(dictionary! {}, b"q /A Do Q".to_vec()));
            let first = doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "MediaBox" => vec![0.into(),0.into(),600.into(),800.into()] });
            let second = doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "Contents" => content });
            doc.objects.insert(root, Object::Dictionary(dictionary! { "Type" => "Pages", "Count" => 2, "Kids" => vec![first.into(),second.into()], "Resources" => dictionary! { "Font" => dictionary! { "P" => font }, "XObject" => dictionary! { "A" => image, "Unused" => unused } } }));
            let root_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root });
            doc.trailer.set("Root", root_id);
            let mut bytes = Vec::new();
            if modern {
                doc.save_modern(&mut bytes).unwrap();
            } else {
                doc.save_to(&mut bytes).unwrap();
            }
            let index = dependency_index(std::io::Cursor::new(bytes.clone())).unwrap();
            let position = |byte: u8| bytes.windows(64).position(|v| v.iter().all(|b| *b == byte)).expect("uncompressed fixture stream") as u64 + 80_000;
            let covers = |ranges: &[PdfByteRange], pos| ranges.iter().any(|r| r.offset <= pos && r.offset + r.length > pos);
            assert!(covers(&index.startup, position(17)), "font program must be warmed globally");
            assert!(!covers(&index.startup, position(88)));
            assert!(index.pages.iter().all(|page| !covers(page, position(88))), "unused shared images must not enter page bundles");
            for byte in [33, 77] {
                assert!(!covers(&index.startup, position(byte)));
                assert!(!covers(&index.pages[0], position(byte)));
                assert!(covers(&index.pages[1], position(byte)), "page bundle needs resource /A and its mask");
            }
        }
    }
}
