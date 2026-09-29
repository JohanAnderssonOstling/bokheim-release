//! Audnexus normalizes Audible data; it is not independent recording evidence.
use super::*;

impl AudibleClient {
    pub(super) async fn audnexus_product(&self, region: &str, asin: &str, source: DiscoverySource) -> Result<Candidate, String> {
        let value = self.json(&self.audnexus_origin, &format!("/books/{asin}"), &[("region", region)]).await?;
        parse_book(&value, region, asin, source)
    }

    pub(super) async fn audnexus_chapters(&self, candidate: &Candidate) -> Result<(Vec<Chapter>, u64), String> {
        let value = self.json(&self.audnexus_origin, &format!("/books/{}/chapters", candidate.asin), &[("region", &candidate.region)]).await?;
        parse_timeline(&value, &candidate.region, &candidate.asin)
    }
}

fn identity(value: &Value, region: &str, asin: &str) -> Result<(), String> {
    if value.get("asin").and_then(Value::as_str) != Some(asin) || value.get("region").and_then(Value::as_str) != Some(region) {
        return Err("Audnexus returned a different ASIN or region".into());
    }
    Ok(())
}

fn parse_book(value: &Value, region: &str, asin: &str, source: DiscoverySource) -> Result<Candidate, String> {
    identity(value, region, asin)?;
    let mut normalized = value.clone();
    for (from, to) in [("publisherName", "publisher_name"), ("releaseDate", "release_date"), ("description", "merchandising_summary"), ("formatType", "format_type"), ("runtimeLengthMin", "runtime_length_min")] {
        if let Some(field) = value.get(from) {
            normalized[to] = field.clone();
        }
    }
    let mut candidate = parse_product(&normalized, region, source)?;
    candidate.metadata_provider = "audnexus".into();
    Ok(candidate)
}

fn parse_timeline(value: &Value, region: &str, asin: &str) -> Result<(Vec<Chapter>, u64), String> {
    identity(value, region, asin)?;
    if value.get("isAccurate").and_then(Value::as_bool) != Some(true) {
        return Err("Audnexus chapter timing is not verified accurate".into());
    }
    let duration = value.get("runtimeLengthMs").and_then(Value::as_u64).ok_or("Audnexus precise runtime missing")?;
    fn nodes(value: &Value, depth: usize, count: &mut usize) -> Result<Value, String> {
        if depth > 32 {
            return Err("Audnexus chapter tree exceeds depth limit".into());
        }
        let input = value.as_array().ok_or("Audnexus chapter list missing")?;
        let mut output = Vec::new();
        for node in input {
            *count += 1;
            if *count > 10_000 {
                return Err("Audnexus chapter tree exceeds size limit".into());
            }
            let mut normalized = serde_json::json!({
                "title": node.get("title"),
                "length_ms": node.get("lengthMs"),
                "start_offset_ms": node.get("startOffsetMs"),
            });
            if let Some(children) = node.get("chapters") {
                normalized["chapters"] = nodes(children, depth + 1, count)?;
            }
            output.push(normalized);
        }
        Ok(Value::Array(output))
    }
    let chapters = nodes(value.get("chapters").ok_or("Audnexus chapters missing")?, 0, &mut 0)?;
    parse_chapters(&serde_json::json!({"content_metadata":{"chapter_info":{
        "runtime_length_ms":duration, "chapters":chapters
    }}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn book_normalization_preserves_recording_evidence_and_provenance() {
        let value = json!({"asin":"B000000001","region":"us","title":"A book",
            "authors":[{"name":"Author"}],"narrators":[{"name":"Narrator"}],
            "publisherName":"Publisher","releaseDate":"2020-01-01","runtimeLengthMin":10,
            "isbn":"9780306406157","formatType":"unabridged"});
        let book = parse_book(&value, "us", "B000000001", DiscoverySource::Api).unwrap();
        assert_eq!(book.metadata_provider, "audnexus");
        assert_eq!(book.publisher.as_deref(), Some("Publisher"));
        assert_eq!(book.abridged, Some(false));
        assert_eq!(book.provider_duration_ms, Some(600_000));
        assert_eq!(book.duration_ms, None, "rounded runtime is never precise evidence");
        assert!(parse_book(&value, "uk", "B000000001", DiscoverySource::Api).is_err());
        assert!(parse_book(&value, "us", "B000000002", DiscoverySource::Api).is_err());
    }

    #[test]
    fn chapter_validation_rejects_inaccurate_partial_and_wrong_recording_data() {
        let mut value = json!({"asin":"B000000001","region":"us","isAccurate":true,"runtimeLengthMs":1000,
            "chapters":[{"title":"Opening","startOffsetMs":0,"lengthMs":500},
                        {"title":"Conclusion","startOffsetMs":500,"lengthMs":500}]});
        assert_eq!(parse_timeline(&value, "us", "B000000001").unwrap().1, 1000);
        assert!(parse_timeline(&value, "uk", "B000000001").is_err());
        value["isAccurate"] = json!(false);
        assert!(parse_timeline(&value, "us", "B000000001").is_err());
        value["isAccurate"] = json!(true);
        value["chapters"][1]["lengthMs"] = json!(400);
        assert!(parse_timeline(&value, "us", "B000000001").is_err());
        value["chapters"][1]["lengthMs"] = json!(500);
        value["chapters"][1]["startOffsetMs"] = json!(400);
        assert!(parse_timeline(&value, "us", "B000000001").is_err());
    }
}
