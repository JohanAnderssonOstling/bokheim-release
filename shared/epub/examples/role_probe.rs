use epub_provider::{CreatorRole, parse_creator_role};
use quick_xml::events::{BytesStart, Event};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::ZipArchive;

const SAMPLE_LIMIT: usize = 4;

#[derive(Clone, Copy)]
enum CreditKind {
    Creator,
    Contributor,
}

impl CreditKind {
    fn label(self) -> &'static str {
        match self {
            Self::Creator => "creator",
            Self::Contributor => "contributor",
        }
    }
}

struct Credit {
    kind: CreditKind,
    id: Option<String>,
    name: String,
    direct_role: Option<String>,
}

struct RoleRefinement {
    target: String,
    value: String,
}

enum Capture {
    Credit { kind: CreditKind, id: Option<String>, direct_role: Option<String>, text: String },
    RoleRefinement { target: String, text: String },
}

#[derive(Default)]
struct RoleStats {
    occurrences: usize,
    files: BTreeSet<PathBuf>,
    samples: Vec<String>,
}

#[derive(Default)]
struct ProbeReport {
    extensions: BTreeMap<String, usize>,
    epub_files: usize,
    parsed_epubs: usize,
    failed_epubs: Vec<(PathBuf, String)>,
    roles: BTreeMap<String, RoleStats>,
    untyped_contributors: RoleStats,
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/home/johan/Hem/syncthing/Böcker"));
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()).into());
    }

    let mut paths = Vec::new();
    collect_files(&root, &mut paths)?;
    paths.sort();

    let mut report = ProbeReport::default();
    for path in paths {
        let extension = path.extension().and_then(|value| value.to_str()).map(str::to_ascii_lowercase).unwrap_or_else(|| "(none)".to_owned());
        *report.extensions.entry(extension.clone()).or_default() += 1;
        if extension != "epub" {
            continue;
        }

        report.epub_files += 1;
        match inspect_epub(&path) {
            Ok((credits, refinements)) => {
                report.parsed_epubs += 1;
                record_roles(&root, &path, credits, refinements, &mut report);
            }
            Err(error) => report.failed_epubs.push((path, error.to_string())),
        }
    }

    print_report(&root, &report);
    Ok(())
}

fn collect_files(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&entry.path(), output)?;
        } else if file_type.is_file() {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn inspect_epub(path: &Path) -> Result<(Vec<Credit>, Vec<RoleRefinement>), Box<dyn Error>> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let opf_path = package_path(&mut archive)?;
    let mut package = String::new();
    archive.by_name(&opf_path)?.read_to_string(&mut package)?;
    Ok(parse_package_roles(&package))
}

fn package_path(archive: &mut ZipArchive<File>) -> Result<String, Box<dyn Error>> {
    let mut container = String::new();
    let read_container = match archive.by_name("META-INF/container.xml") {
        Ok(mut file) => file.read_to_string(&mut container).is_ok(),
        Err(_) => false,
    };
    if read_container {
        let mut reader = quick_xml::Reader::from_str(&container);
        reader.trim_text(true);
        let mut buffer = Vec::new();
        loop {
            match reader.read_event_into(&mut buffer) {
                Ok(Event::Start(element)) | Ok(Event::Empty(element)) if local_name(element.name().as_ref()) == b"rootfile" => {
                    if let Some(path) = attribute(&element, b"full-path") {
                        return Ok(path);
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buffer.clear();
        }
    }

    archive.file_names().find(|name| name.to_ascii_lowercase().ends_with(".opf")).map(str::to_owned).ok_or_else(|| "EPUB has no package document".into())
}

fn parse_package_roles(package: &str) -> (Vec<Credit>, Vec<RoleRefinement>) {
    let mut reader = quick_xml::Reader::from_str(package);
    reader.trim_text(false);
    let mut buffer = Vec::new();
    let mut capture = None;
    let mut credits = Vec::new();
    let mut refinements = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"creator" || name == b"contributor" {
                    capture = Some(Capture::Credit { kind: if name == b"creator" { CreditKind::Creator } else { CreditKind::Contributor }, id: attribute(&element, b"id"), direct_role: attribute(&element, b"role"), text: String::new() });
                } else if name == b"meta" && attribute(&element, b"property").as_deref().is_some_and(is_role_property) {
                    if let Some(target) = attribute(&element, b"refines").and_then(normalized_target) {
                        if let Some(value) = attribute(&element, b"content") {
                            refinements.push(RoleRefinement { target, value });
                        } else {
                            capture = Some(Capture::RoleRefinement { target, text: String::new() });
                        }
                    }
                }
            }
            Ok(Event::Empty(element)) => {
                if local_name(element.name().as_ref()) == b"meta" && attribute(&element, b"property").as_deref().is_some_and(is_role_property) {
                    if let (Some(target), Some(value)) = (attribute(&element, b"refines").and_then(normalized_target), attribute(&element, b"content")) {
                        refinements.push(RoleRefinement { target, value });
                    }
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(capture) = &mut capture {
                    let decoded = text.unescape().map(|value| value.into_owned()).unwrap_or_default();
                    match capture {
                        Capture::Credit { text, .. } | Capture::RoleRefinement { text, .. } => text.push_str(&decoded),
                    }
                }
            }
            Ok(Event::CData(text)) => {
                if let Some(capture) = &mut capture {
                    let decoded = String::from_utf8_lossy(text.as_ref());
                    match capture {
                        Capture::Credit { text, .. } | Capture::RoleRefinement { text, .. } => text.push_str(&decoded),
                    }
                }
            }
            Ok(Event::End(element)) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let finishes_capture = matches!((&capture, name), (Some(Capture::Credit { .. }), b"creator" | b"contributor") | (Some(Capture::RoleRefinement { .. }), b"meta"));
                if finishes_capture {
                    match capture.take().expect("capture exists") {
                        Capture::Credit { kind, id, direct_role, text } => credits.push(Credit { kind, id, name: compact_whitespace(&text), direct_role }),
                        Capture::RoleRefinement { target, text } => refinements.push(RoleRefinement { target, value: compact_whitespace(&text) }),
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }

    (credits, refinements)
}

fn record_roles(root: &Path, path: &Path, credits: Vec<Credit>, refinements: Vec<RoleRefinement>, report: &mut ProbeReport) {
    let mut refined_roles = BTreeMap::<String, Vec<String>>::new();
    for refinement in refinements {
        if !refinement.value.trim().is_empty() {
            refined_roles.entry(refinement.target).or_default().push(refinement.value);
        }
    }

    for credit in credits {
        let mut roles = credit.direct_role.into_iter().collect::<Vec<_>>();
        if let Some(id) = &credit.id {
            roles.extend(refined_roles.get(id).into_iter().flatten().cloned());
        }
        if roles.is_empty() {
            match credit.kind {
                CreditKind::Creator => roles.push("(implicit author)".to_owned()),
                CreditKind::Contributor => {
                    record_stat(&mut report.untyped_contributors, root, path, credit.kind, &credit.name);
                    continue;
                }
            }
        }

        for role in roles {
            let role = compact_whitespace(&role).to_ascii_lowercase();
            if role.is_empty() {
                continue;
            }
            let stats = report.roles.entry(role).or_default();
            record_stat(stats, root, path, credit.kind, &credit.name);
        }
    }
}

fn record_stat(stats: &mut RoleStats, root: &Path, path: &Path, kind: CreditKind, name: &str) {
    stats.occurrences += 1;
    stats.files.insert(path.to_owned());
    if stats.samples.len() < SAMPLE_LIMIT {
        let relative = path.strip_prefix(root).unwrap_or(path);
        stats.samples.push(format!("{} | {} | {}", kind.label(), name, relative.display()));
    }
}

fn print_report(root: &Path, report: &ProbeReport) {
    println!("EPUB role probe: {}", root.display());
    println!("Files by extension:");
    for (extension, count) in &report.extensions {
        println!("  {extension}: {count}");
    }
    println!("EPUBs: {} total, {} parsed, {} failed", report.epub_files, report.parsed_epubs, report.failed_epubs.len());
    println!();
    println!("Observed role values:");
    for (raw, stats) in &report.roles {
        let token = canonical_role_token(raw);
        println!("  {raw:?} -> token={token:?}, current={} | {} credits in {} files", role_label(parse_creator_role(raw)), stats.occurrences, stats.files.len(),);
        for sample in &stats.samples {
            println!("    {sample}");
        }
    }
    if report.untyped_contributors.occurrences > 0 {
        println!();
        println!("Untyped dc:contributor values: {} credits in {} files", report.untyped_contributors.occurrences, report.untyped_contributors.files.len(),);
        for sample in &report.untyped_contributors.samples {
            println!("  {sample}");
        }
    }
    if !report.failed_epubs.is_empty() {
        println!();
        println!("Failed EPUBs:");
        for (path, error) in &report.failed_epubs {
            println!("  {}: {error}", path.display());
        }
    }
}

fn attribute(element: &BytesStart<'_>, wanted: &[u8]) -> Option<String> {
    element.attributes().flatten().find(|attribute| local_name(attribute.key.as_ref()) == wanted).map(|attribute| String::from_utf8_lossy(attribute.value.as_ref()).into_owned())
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn is_role_property(property: &str) -> bool {
    property.rsplit(':').next().is_some_and(|name| name.eq_ignore_ascii_case("role"))
}

fn normalized_target(value: String) -> Option<String> {
    let value = value.trim().trim_start_matches('#');
    (!value.is_empty()).then(|| value.to_owned())
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn canonical_role_token(value: &str) -> &str {
    if value == "(implicit author)" {
        return "aut";
    }
    value.trim().trim_end_matches('/').rsplit(['/', '#', ':']).next().unwrap_or(value)
}

fn role_label(role: CreatorRole) -> String {
    role.as_str().to_owned()
}
