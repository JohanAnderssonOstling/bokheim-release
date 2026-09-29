use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use html::resources::ResourceProvider;
use html::style::property_name_is_supported;
use html_book::{BookSourceFormat, BookSource};
use lightningcss::printer::PrinterOptions;
use lightningcss::properties::Property;
use lightningcss::rules::CssRule;
use lightningcss::stylesheet::{ParserOptions, StyleSheet};
use lightningcss::traits::ToCss;
use scraper::{Html, Selector};

/// Tracks count and which books contain an unsupported property
struct PropertyInfo {
    count: usize,
    books: HashSet<String>,
}

impl PropertyInfo {
    fn new() -> Self {
        Self { count: 0, books: HashSet::new() }
    }
}

fn main() {
    let Some(root) = env::args().nth(1) else {
        eprintln!("Usage: css_audit <path>");
        std::process::exit(1);
    };
    let root_path = Path::new(&root);
    let mut epubs = Vec::new();
    collect_epubs(root_path, &mut epubs);
    if epubs.is_empty() {
        println!("No EPUBs found under {}", root);
        return;
    }

    let max_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let worker_count = max_threads.min(epubs.len()).max(1);

    let root_path = Arc::new(root_path.to_path_buf());
    let epubs = Arc::new(epubs);
    let index = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let root_path = Arc::clone(&root_path);
        let epubs = Arc::clone(&epubs);
        let index = Arc::clone(&index);
        let completed = Arc::clone(&completed);
        handles.push(std::thread::spawn(move || {
            let mut local: HashMap<String, PropertyInfo> = HashMap::new();
            loop {
                let idx = index.fetch_add(1, Ordering::Relaxed);
                if idx >= epubs.len() {
                    break;
                }
                let rel_path = epubs[idx].strip_prefix(root_path.as_path()).unwrap_or(&epubs[idx]).to_string_lossy().to_string();
                merge_counts(&mut local, audit_epub(&epubs[idx], &rel_path));
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                print_progress(done, epubs.len());
            }
            local
        }));
    }

    let mut unsupported: HashMap<String, PropertyInfo> = HashMap::new();
    for handle in handles {
        if let Ok(local) = handle.join() {
            merge_counts(&mut unsupported, local);
        }
    }

    println!();
    if unsupported.is_empty() {
        println!("All CSS properties are supported.");
    } else {
        let mut props: Vec<_> = unsupported.into_iter().collect();
        props.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(&b.0)));
        for (prop, info) in props {
            println!("Unsupported ({}): {}", info.count, prop);
            let mut books: Vec<_> = info.books.into_iter().collect();
            books.sort();
            for book in books {
                println!("    {}", book);
            }
        }
    }
}

fn collect_epubs(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            collect_epubs(&entry.path(), out);
        }
        return;
    }

    let is_epub = path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.eq_ignore_ascii_case("epub")).unwrap_or(false);
    if is_epub {
        out.push(path.to_path_buf());
    }
}

fn audit_epub(path: &Path, rel_path: &str) -> HashMap<String, PropertyInfo> {
    let mut unsupported: HashMap<String, PropertyInfo> = HashMap::new();
    let Ok(file) = fs::File::open(path) else {
        return unsupported;
    };
    let Ok(book) = BookSource::from_reader(BookSourceFormat::Epub, file) else {
        return unsupported;
    };
    let provider = book.provider();
    let html_entries = book.document_uris().to_vec();

    let mut seen_css_uris = HashSet::new();

    for html_entry in html_entries {
        let Ok(html) = provider.read_string(&html_entry) else {
            continue;
        };

        let css_sources = collect_css_from_html(&html, provider.as_ref(), &html_entry, &mut seen_css_uris);
        for (source, css) in css_sources {
            let sheet = match StyleSheet::parse(&css, ParserOptions::default()) {
                Ok(sheet) => sheet,
                Err(err) => {
                    eprintln!("{}: {}: failed to parse CSS: {}", path.display(), source, err);
                    continue;
                }
            };
            collect_unsupported_properties(&sheet, &mut unsupported, rel_path);
        }
    }

    unsupported
}

fn collect_css_from_html(html: &str, provider: &dyn ResourceProvider, base_uri: &str, seen_uris: &mut HashSet<String>) -> Vec<(String, String)> {
    let doc = Html::parse_document(html);
    let mut out = Vec::new();

    if let Ok(selector) = Selector::parse("style") {
        for (idx, style) in doc.select(&selector).enumerate() {
            let text = style.text().collect::<String>();
            if !text.trim().is_empty() {
                out.push((format!("{base_uri}#style[{idx}]"), text));
            }
        }
    }

    if let Ok(selector) = Selector::parse("link[rel=\"stylesheet\"], link[rel=\"StyleSheet\"]") {
        for link in doc.select(&selector) {
            if let Some(href) = link.value().attr("href") {
                let uri = provider.resolve(base_uri, href);
                if !seen_uris.insert(uri.clone()) {
                    continue;
                }
                if let Ok(css) = provider.read_string(&uri) {
                    out.push((uri, css));
                }
            }
        }
    }

    out
}

fn collect_unsupported_properties(sheet: &StyleSheet<'_>, out: &mut HashMap<String, PropertyInfo>, book_path: &str) {
    for rule in &sheet.rules.0 {
        let CssRule::Style(style_rule) = rule else {
            continue;
        };
        for property in &style_rule.declarations.declarations {
            let name = property_name(property);
            if property_name_is_supported(&name) {
                continue;
            }
            let info = out.entry(name).or_insert_with(PropertyInfo::new);
            info.count += 1;
            info.books.insert(book_path.to_string());
        }
    }
}

fn property_name(property: &Property) -> String {
    property.property_id().to_css_string(PrinterOptions::default()).unwrap_or_else(|_| "<property>".to_string())
}

fn merge_counts(target: &mut HashMap<String, PropertyInfo>, source: HashMap<String, PropertyInfo>) {
    for (key, value) in source {
        let info = target.entry(key).or_insert_with(PropertyInfo::new);
        info.count += value.count;
        info.books.extend(value.books);
    }
}

fn print_progress(done: usize, total: usize) {
    print!("\rScanned {done} / {total}");
    let _ = io::stdout().flush();
}
