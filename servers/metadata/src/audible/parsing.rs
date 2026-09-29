use super::*;

pub(super) fn valid_asin(value: &str) -> bool {
    value.len() == 10 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn names(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).into_iter().flatten().take(32).filter_map(|person| person.get("name").and_then(Value::as_str)).filter(|name| !name.trim().is_empty()).map(|name| name.chars().take(512).collect()).collect()
}

pub(super) fn parse_product(value: &Value, region: &str, source: DiscoverySource) -> Result<Candidate, String> {
    let asin = value.get("asin").and_then(Value::as_str).filter(|s| valid_asin(s)).ok_or("Invalid Audible ASIN")?;
    let title = value.get("title").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or("Audible title missing")?;
    let text = |field: &str| value.get(field).and_then(Value::as_str).map(|s| s.chars().take(4096).collect::<String>());
    // Only explicit product ISBN fields are eligible. Never collect unrelated
    // ISBNs from descriptions, suggestions, or marketing links.
    let mut isbns = Vec::new();
    for field in ["isbn", "isbn10", "isbn13", "isbn_10", "isbn_13"] {
        if let Some(isbn) = value.get(field).and_then(Value::as_str).and_then(valid_isbn) {
            if !isbns.contains(&isbn) {
                isbns.push(isbn);
            }
        }
    }
    let abridged = value.get("is_abridged").and_then(Value::as_bool).or_else(|| match value.get("format_type").and_then(Value::as_str) {
        Some("abridged") => Some(true),
        Some("unabridged") => Some(false),
        _ => None,
    });
    Ok(Candidate {
        asin: asin.to_ascii_uppercase(),
        region: region.into(),
        title: title.chars().take(4096).collect(),
        subtitle: text("subtitle"),
        authors: names(value.get("authors")),
        narrators: names(value.get("narrators")),
        publisher: text("publisher_name"),
        description: value.get("merchandising_summary").and_then(Value::as_str).map(|s| s.chars().take(32_768).collect()),
        release_date: text("release_date"),
        isbns,
        abridged,
        provider_duration_ms: value.get("runtime_length_min").and_then(Value::as_u64).and_then(|m| m.checked_mul(60_000)),
        duration_ms: None,
        chapters: Vec::new(),
        source,
        metadata_provider: "audible".into(),
        chapter_provider: None,
    })
}

fn valid_isbn(raw: &str) -> Option<String> {
    let value: String = raw.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_ascii_uppercase();
    let b = value.as_bytes();
    let valid = match b.len() {
        13 if b.iter().all(u8::is_ascii_digit) && (value.starts_with("978") || value.starts_with("979")) => b.iter().enumerate().map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 }).sum::<u32>() % 10 == 0,
        10 if b[..9].iter().all(u8::is_ascii_digit) && (b[9].is_ascii_digit() || b[9] == b'X') => b.iter().enumerate().map(|(i, b)| (10 - i) as u32 * if *b == b'X' { 10 } else { u32::from(b - b'0') }).sum::<u32>() % 11 == 0,
        _ => false,
    };
    valid.then_some(value)
}

pub(super) fn parse_chapters(value: &Value) -> Result<(Vec<Chapter>, u64), String> {
    let info = value.pointer("/content_metadata/chapter_info").ok_or("Audible chapter info missing")?;
    let roots = info.get("chapters").and_then(Value::as_array).ok_or("Audible chapter tree missing")?;
    let mut stack = roots.iter().rev().map(|node| (node, 0)).collect::<Vec<_>>();
    let mut chapters = Vec::new();
    let mut visited = 0;
    while let Some((node, depth)) = stack.pop() {
        visited += 1;
        if visited > 10_000 || depth > 32 {
            return Err("Audible chapter tree exceeds limits".into());
        }
        if let Some(children) = node.get("chapters").and_then(Value::as_array) {
            stack.extend(children.iter().rev().map(|child| (child, depth + 1)));
        }
        let length = node.get("length_ms").and_then(Value::as_u64).unwrap_or(0);
        if length == 0 {
            continue;
        }
        let start = node.get("start_offset_ms").and_then(Value::as_u64).ok_or("Chapter start missing")?;
        let end = start.checked_add(length).ok_or("Chapter duration overflow")?;
        let title = node.get("title").and_then(Value::as_str).unwrap_or("").chars().take(4096).collect();
        chapters.push(Chapter { title, start_ms: start, end_ms: end });
    }
    chapters.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then(b.end_ms.cmp(&a.end_ms)));
    chapters.dedup_by(|a, b| a.start_ms == b.start_ms && a.end_ms == b.end_ms && a.title == b.title);
    // Whole-section containers precede contained children in this order.
    // Separately spoken headings end before their children and are retained.
    let leaves =
        chapters.iter().enumerate().filter(|(i, c)| !chapters.get(i + 1).is_some_and(|d| c.start_ms <= d.start_ms && d.end_ms <= c.end_ms && (c.start_ms != d.start_ms || c.end_ms != d.end_ms))).map(|(_, c)| c.clone()).collect::<Vec<_>>();
    if leaves.is_empty() || leaves.windows(2).any(|w| w[0].end_ms > w[1].start_ms) || leaves.last().is_some_and(|c| c.end_ms > 365 * 24 * 3600 * 1000) {
        return Err("Invalid or overlapping Audible chapter timeline".into());
    }
    let duration = leaves.last().unwrap().end_ms;
    // A child list may omit audio covered by its parent. Such a partial tree
    // must not become precise runtime evidence or replacement boundaries.
    let tree_end = chapters.iter().map(|c| c.end_ms).max().unwrap();
    if leaves[0].start_ms != 0 || tree_end.abs_diff(duration) > 1 || leaves.windows(2).any(|w| w[1].start_ms.abs_diff(w[0].end_ms) > 1) {
        return Err("Incomplete Audible chapter coverage".into());
    }
    if let Some(declared) = info.get("runtime_length_ms") {
        let declared = declared.as_u64().ok_or("Invalid Audible runtime")?;
        if declared.abs_diff(duration) > 1 {
            return Err("Audible chapter timeline disagrees with declared runtime".into());
        }
    }
    Ok((leaves, duration))
}

pub(super) fn website_asins(html: &str) -> Result<Vec<String>, String> {
    let lower = html.to_ascii_lowercase();
    if lower.contains("captcha") || lower.contains("robot check") {
        return Err("Audible website unavailable".into());
    }
    if !lower.contains("<html") || !lower.contains("<title") || !lower.contains("search") {
        return Err("Unexpected Audible website response".into());
    }
    let mut asins = Vec::new();
    // Read only product links. Query parameters and arbitrary 10-character
    // strings elsewhere in the document are not identifiers.
    for part in html.split("/pd/").skip(1) {
        let path = part.split(['?', '#', '"', '\'', '<', '>', '&', ' ']).next().unwrap_or_default();
        if let Some(asin) = path.trim_end_matches('/').rsplit('/').next().filter(|segment| valid_asin(segment)) {
            let asin = asin.to_ascii_uppercase();
            if !asins.contains(&asin) {
                asins.push(asin);
            }
        }
        if asins.len() == 32 {
            break;
        }
    }
    Ok(asins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn website_identifiers_come_from_product_paths() {
        assert_eq!(website_asins("<html><title>Search</title><a href='/pd/Book/B000000001?ref=x'>Book</a><a href='/pd/B000000002'>Another</a> B000000003</html>").unwrap(), ["B000000001", "B000000002"]);
        assert!(website_asins("<html><title>Search</title>captcha</html>").is_err());
        assert!(website_asins("<html><title>Sign in</title></html>").is_err());
    }
    #[test]
    fn nested_containers_do_not_duplicate_audio_and_spoken_headings_survive() {
        let value = json!({"content_metadata":{"chapter_info":{"chapters":[
            {"title":"Part 1","start_offset_ms":0,"length_ms":1000,"chapters":[
                {"title":"First chapter","start_offset_ms":0,"length_ms":500},
                {"title":"Second chapter","start_offset_ms":500,"length_ms":500}]},
            {"title":"Part 2 heading","start_offset_ms":1000,"length_ms":50,"chapters":[
                {"title":"Third chapter","start_offset_ms":1050,"length_ms":500}]}
        ]}}});
        let (chapters, duration) = parse_chapters(&value).unwrap();
        assert_eq!(duration, 1550);
        assert_eq!(chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["First chapter", "Second chapter", "Part 2 heading", "Third chapter"]);
    }
    #[test]
    fn explicit_isbns_are_checksum_validated() {
        let c = parse_product(&json!({"asin":"B000000001","title":"Book","isbn":"0-306-40615-2","isbn13":"9780306406158","description":"ISBN 9780306406157"}), "us", DiscoverySource::Api).unwrap();
        assert_eq!(c.isbns, ["0306406152"]);
    }
}

#[cfg(test)]
mod live_fixture_tests {
    use super::*;
    #[test]
    fn observed_audible_response_keeps_precise_chapters_and_isbn() {
        let product: Value = serde_json::from_str(include_str!("fixtures/why-nothing-works-product.json")).unwrap();
        let tree: Value = serde_json::from_str(include_str!("fixtures/why-nothing-works-chapters.json")).unwrap();
        let candidate = parse_product(&product, "us", DiscoverySource::Api).unwrap();
        assert_eq!(candidate.isbns, ["9781668651223"]);
        let (chapters, duration) = parse_chapters(&tree).unwrap();
        assert_eq!(chapters.len(), 22);
        assert_eq!(duration, 48_731_333);
        assert!(chapters.windows(2).all(|w| w[0].end_ms <= w[1].start_ms));
        assert!(chapters.iter().filter(|c| contract::chapters::descriptive(&c.title, &candidate.title)).count() >= 17);
    }
}

#[cfg(test)]
mod asin_path_regressions {
    use super::*;
    #[test]
    fn ten_character_book_slug_is_not_the_asin() {
        assert_eq!(website_asins("<html><title>Search</title><a href='/pd/Foundation/B000000002'>Foundation</a></html>").unwrap(), ["B000000002"]);
        assert_eq!(website_asins("<html><title>Search</title><a href='/pd/Foundation/not-valid/'>Book</a></html>").unwrap(), Vec::<String>::new());
        assert_eq!(website_asins("<html><title>Search</title><a href='/pd/B000000002/?ref=FOUNDATION'>Book</a></html>").unwrap(), ["B000000002"]);
    }
}

#[cfg(test)]
mod coverage_regressions {
    use super::*;
    use serde_json::json;
    #[test]
    fn partial_children_and_declared_runtime_mismatches_are_rejected() {
        let mut value = json!({"content_metadata":{"chapter_info":{"runtime_length_ms":120000,"chapters":[
            {"title":"Whole book","start_offset_ms":0,"length_ms":120000,"chapters":[
                {"title":"Opening","start_offset_ms":0,"length_ms":60000},
                {"title":"Main story","start_offset_ms":60000,"length_ms":30000}]}]}}});
        assert!(parse_chapters(&value).is_err());
        value["content_metadata"]["chapter_info"]["chapters"][0]["chapters"][1]["length_ms"] = json!(60000);
        assert_eq!(parse_chapters(&value).unwrap().1, 120000);
        value["content_metadata"]["chapter_info"]["runtime_length_ms"] = json!(150000);
        assert!(parse_chapters(&value).is_err());
        value["content_metadata"]["chapter_info"]["runtime_length_ms"] = json!(120000);
        value["content_metadata"]["chapter_info"]["chapters"][0]["chapters"][0]["length_ms"] = json!(50000);
        assert!(parse_chapters(&value).is_err());
        value["content_metadata"]["chapter_info"].as_object_mut().unwrap().remove("runtime_length_ms");
        assert!(parse_chapters(&value).is_err());
    }
}
