//! Count explicitly declared subjects and genres in EPUB package metadata.
//!
//! This probe does not infer categories from titles, descriptions, directory
//! names, or book content. It reads `dc:subject`, subject/genre `<meta>`
//! properties, and non-empty subject/genre/tag values in Calibre custom
//! metadata.
//!
//! ```text
//! cargo run -p epub_provider --example subject_genre_probe
//! cargo run -p epub_provider --example subject_genre_probe -- /path/to/books
//! ```

use quick_xml::events::{BytesStart, Event};
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::ZipArchive;

const DEFAULT_BOOK_DIRECTORY: &str = "/home/johan/Hem/syncthing/Böcker";

struct DeclaredTerm {
    value: String,
    source: String,
}

enum Capture {
    Subject { source: String, text: String },
    Meta { field: String, text: String },
}

#[derive(Default)]
struct SubjectStats {
    occurrences: usize,
    epubs: BTreeSet<PathBuf>,
}

#[derive(Default)]
struct ProbeReport {
    epub_files: usize,
    parsed_epubs: usize,
    epubs_without_declared_terms: usize,
    subjects: BTreeMap<String, SubjectStats>,
    sources: BTreeMap<String, usize>,
    failed_epubs: Vec<(PathBuf, String)>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_BOOK_DIRECTORY));
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()).into());
    }

    let mut paths = Vec::new();
    collect_epubs(&root, &mut paths)?;
    paths.sort();

    let mut report = ProbeReport { epub_files: paths.len(), ..ProbeReport::default() };
    for path in paths {
        match inspect_epub(&path) {
            Ok(terms) => record_terms(&path, terms, &mut report),
            Err(error) => report.failed_epubs.push((path, error.to_string())),
        }
    }

    print_report(&root, &report);
    Ok(())
}

fn collect_epubs(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_epubs(&entry.path(), output)?;
        } else if file_type.is_file() && has_epub_extension(&entry.path()) {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn has_epub_extension(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
}

fn inspect_epub(path: &Path) -> Result<Vec<DeclaredTerm>, Box<dyn Error>> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let opf_path = package_path(&mut archive)?;
    let mut package = String::new();
    archive.by_name(&opf_path)?.read_to_string(&mut package)?;
    parse_package_terms(&package)
}

fn package_path(archive: &mut ZipArchive<File>) -> Result<String, Box<dyn Error>> {
    let mut container = String::new();
    archive.by_name("META-INF/container.xml")?.read_to_string(&mut container)?;
    let mut reader = quick_xml::Reader::from_str(&container);
    reader.trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element) if local_name(element.name().as_ref()) == b"rootfile" => {
                if let Some(path) = attribute(&element, b"full-path") {
                    return Ok(path);
                }
            }
            Event::Eof => return Err("EPUB container has no package rootfile".into()),
            _ => {}
        }
        buffer.clear();
    }
}

fn parse_package_terms(package: &str) -> Result<Vec<DeclaredTerm>, Box<dyn Error>> {
    let mut reader = quick_xml::Reader::from_str(package);
    reader.trim_text(false);
    let mut buffer = Vec::new();
    let mut in_metadata = false;
    let mut capture = None;
    let mut terms = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"metadata" {
                    in_metadata = true;
                } else if in_metadata && name == b"subject" {
                    capture = Some(Capture::Subject { source: String::from_utf8_lossy(qualified_name.as_ref()).into_owned(), text: String::new() });
                } else if in_metadata && name == b"meta" {
                    begin_meta(&element, &mut capture, &mut terms);
                }
            }
            Event::Empty(element) if in_metadata && local_name(element.name().as_ref()) == b"meta" => {
                begin_meta(&element, &mut capture, &mut terms);
                finish_capture(&mut capture, &mut terms);
            }
            Event::Text(text) => {
                if let Some(capture) = &mut capture {
                    append_capture(capture, &text.unescape().map(|value| value.into_owned()).unwrap_or_default());
                }
            }
            Event::CData(text) => {
                if let Some(capture) = &mut capture {
                    append_capture(capture, &String::from_utf8_lossy(text.as_ref()));
                }
            }
            Event::End(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let completes_capture = matches!((&capture, name), (Some(Capture::Subject { .. }), b"subject") | (Some(Capture::Meta { .. }), b"meta"));
                if completes_capture {
                    finish_capture(&mut capture, &mut terms);
                }
                if name == b"metadata" {
                    in_metadata = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    Ok(terms)
}

fn begin_meta(element: &BytesStart<'_>, capture: &mut Option<Capture>, terms: &mut Vec<DeclaredTerm>) {
    let Some(field) = attribute(element, b"property").or_else(|| attribute(element, b"name")) else { return };
    if !is_relevant_meta_field(&field) {
        return;
    }
    if let Some(content) = attribute(element, b"content") {
        extract_meta_terms(&field, &content, terms);
    } else {
        *capture = Some(Capture::Meta { field, text: String::new() });
    }
}

fn append_capture(capture: &mut Capture, value: &str) {
    match capture {
        Capture::Subject { text, .. } | Capture::Meta { text, .. } => text.push_str(value),
    }
}

fn finish_capture(capture: &mut Option<Capture>, terms: &mut Vec<DeclaredTerm>) {
    match capture.take() {
        Some(Capture::Subject { source, text }) => push_plain_term(source, &text, terms),
        Some(Capture::Meta { field, text }) => extract_meta_terms(&field, &text, terms),
        None => {}
    }
}

fn is_relevant_meta_field(field: &str) -> bool {
    field.eq_ignore_ascii_case("calibre:user_metadata") || is_subject_or_genre_name(field)
}

fn is_subject_or_genre_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("subject") || name.contains("genre") || name.split(|character: char| !character.is_alphanumeric()).any(|token| token == "tag" || token == "tags") || name.ends_with("tags")
}

fn extract_meta_terms(field: &str, raw: &str, terms: &mut Vec<DeclaredTerm>) {
    if field.eq_ignore_ascii_case("calibre:user_metadata") {
        extract_calibre_user_metadata(field, raw, terms);
    } else if field.to_ascii_lowercase().contains("subject") {
        push_plain_term(field.to_owned(), raw, terms);
    } else {
        extract_encoded_value(field, raw, terms);
    }
}

fn extract_calibre_user_metadata(field: &str, raw: &str, terms: &mut Vec<DeclaredTerm>) {
    let Ok(Value::Object(properties)) = serde_json::from_str(raw) else { return };
    for (key, property) in properties {
        let Value::Object(property) = property else { continue };
        let identity = format!("{} {} {}", key, property.get("name").and_then(Value::as_str).unwrap_or_default(), property.get("label").and_then(Value::as_str).unwrap_or_default());
        if !is_subject_or_genre_name(&identity) {
            continue;
        }
        if let Some(value) = property.get("#value#") {
            push_json_terms(format!("{field}/{key}"), value, terms);
        }
    }
}

fn extract_encoded_value(field: &str, raw: &str, terms: &mut Vec<DeclaredTerm>) {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(object)) => {
            if let Some(value) = object.get("#value#") {
                push_json_terms(field.to_owned(), value, terms);
            }
        }
        Ok(value @ Value::Array(_)) | Ok(value @ Value::String(_)) => push_json_terms(field.to_owned(), &value, terms),
        Ok(_) => {}
        Err(_) => push_plain_term(field.to_owned(), raw, terms),
    }
}

fn push_json_terms(source: String, value: &Value, terms: &mut Vec<DeclaredTerm>) {
    match value {
        Value::String(value) => push_plain_term(source, value, terms),
        Value::Array(values) => {
            for value in values {
                push_json_terms(source.clone(), value, terms);
            }
        }
        _ => {}
    }
}

fn push_plain_term(source: String, value: &str, terms: &mut Vec<DeclaredTerm>) {
    let value = compact_whitespace(value);
    if !value.is_empty() {
        terms.push(DeclaredTerm { value, source });
    }
}

fn record_terms(path: &Path, terms: Vec<DeclaredTerm>, report: &mut ProbeReport) {
    report.parsed_epubs += 1;
    if terms.is_empty() {
        report.epubs_without_declared_terms += 1;
        return;
    }

    for term in terms {
        *report.sources.entry(term.source).or_default() += 1;
        let stats = report.subjects.entry(term.value).or_default();
        stats.occurrences += 1;
        stats.epubs.insert(path.to_owned());
    }
}

fn attribute(element: &BytesStart<'_>, wanted: &[u8]) -> Option<String> {
    element.attributes().flatten().find(|attribute| local_name(attribute.key.as_ref()) == wanted).and_then(|attribute| attribute.unescape_value().ok().map(|value| value.into_owned()))
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn print_report(root: &Path, report: &ProbeReport) {
    println!("EPUB declared subject/genre probe: {}", root.display());
    println!(
        "EPUBs: {} found, {} parsed, {} failed, {} without declared subject/genre metadata, {} distinct values",
        report.epub_files,
        report.parsed_epubs,
        report.failed_epubs.len(),
        report.epubs_without_declared_terms,
        report.subjects.len()
    );
    println!("Declared metadata sources:");
    for (source, count) in &report.sources {
        println!("  {source}: {count}");
    }
    println!();
    println!("Occurrences  EPUBs  Declared subject/genre");

    let mut subjects = report.subjects.iter().collect::<Vec<_>>();
    subjects.sort_by_key(|(subject, stats)| (Reverse(stats.occurrences), subject.to_lowercase(), (*subject).clone()));
    for (subject, stats) in subjects {
        println!("{:>11}  {:>5}  {}", stats.occurrences, stats.epubs.len(), subject);
    }

    if !report.failed_epubs.is_empty() {
        eprintln!();
        eprintln!("Failed EPUBs:");
        for (path, error) in &report.failed_epubs {
            eprintln!("  {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(value: &str) -> DeclaredTerm {
        DeclaredTerm { value: value.to_owned(), source: "test".to_owned() }
    }

    #[test]
    fn epub_extensions_are_case_insensitive() {
        assert!(has_epub_extension(Path::new("book.epub")));
        assert!(has_epub_extension(Path::new("book.EPUB")));
        assert!(!has_epub_extension(Path::new("book.pdf")));
    }

    #[test]
    fn extracts_explicit_terms_from_supported_metadata_properties() {
        let package = r###"<package xmlns:dc="dc"><metadata>
            <dc:subject>Science Fiction</dc:subject>
            <meta property="se:subject">Fiction</meta>
            <meta name="calibre:user_metadata:#genre" content='{&quot;#value#&quot;:[&quot;Mystery&quot;,&quot;Crime&quot;]}'/>
            <meta property="calibre:user_metadata"><![CDATA[{
                "#fast":{"name":"FAST Tags","label":"fast","#value#":["Robots"]},
                "#notes":{"name":"Notes","label":"notes","#value#":"ignore me"}
            }]]></meta>
            <meta name="calibre:user_metadata:#lc_genre" content='{"#value#":null}'/>
        </metadata></package>"###;

        let terms = parse_package_terms(package).unwrap();
        let values = terms.into_iter().map(|term| term.value).collect::<Vec<_>>();
        assert_eq!(values, ["Science Fiction", "Fiction", "Mystery", "Crime", "Robots"]);
    }

    #[test]
    fn repeated_terms_count_occurrences_and_distinct_epubs() {
        let mut report = ProbeReport::default();
        record_terms(Path::new("one.epub"), vec![term("Fiction"), term("Fiction")], &mut report);
        record_terms(Path::new("two.epub"), vec![term("Fiction")], &mut report);

        let fiction = &report.subjects["Fiction"];
        assert_eq!(fiction.occurrences, 3);
        assert_eq!(fiction.epubs.len(), 2);
    }
}
