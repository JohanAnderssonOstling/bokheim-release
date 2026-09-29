//! Bounded Audible discovery. No library paths, database connections or credentials.
use futures_util::StreamExt;
use metadata_contract::audible::{self as contract, *};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};

mod audnexus;
mod covers;
mod parsing;
use parsing::{parse_chapters, parse_product, website_asins};

const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const CANDIDATE_LIMIT: usize = 24;
const GROUPS: &str = "product_desc,product_attrs,contributors,product_extended_attrs,product_details";

#[derive(Clone)]
pub(crate) struct AudibleClient {
    http: reqwest::Client,
    origins: Arc<Vec<(String, String, String)>>,
    audnexus_origin: String,
    cache: Arc<Mutex<HashMap<String, (Instant, Arc<Vec<u8>>)>>>,
    requests: Arc<Mutex<Option<Instant>>>,
    lookups: Arc<Semaphore>,
}

impl AudibleClient {
    pub(crate) fn new() -> Self {
        Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(8)).redirect(reqwest::redirect::Policy::none()).user_agent("Bokheim metadata enrichment").build().expect("HTTP client"),
            origins: Arc::new(vec![("us".into(), "https://api.audible.com".into(), "https://www.audible.com".into()), ("uk".into(), "https://api.audible.co.uk".into(), "https://www.audible.co.uk".into())]),
            audnexus_origin: "https://api.audnex.us".into(),
            cache: Arc::default(),
            requests: Arc::default(),
            lookups: Arc::new(Semaphore::new(2)),
        }
    }

    async fn get(&self, url: reqwest::Url) -> Result<Arc<Vec<u8>>, String> {
        let key = url.to_string();
        if let Some((at, bytes)) = self.cache.lock().await.get(&key) {
            if at.elapsed() < Duration::from_secs(6 * 3600) {
                return Ok(bytes.clone());
            }
        }
        {
            let mut previous = self.requests.lock().await;
            if let Some(at) = *previous {
                tokio::time::sleep(Duration::from_millis(500).saturating_sub(at.elapsed())).await;
            }
            *previous = Some(Instant::now());
        }
        let response = self.http.get(url).send().await.map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("{} HTTP {}", response.url().host_str().unwrap_or("audiobook provider"), response.status()));
        }
        if response.content_length().is_some_and(|n| n > RESPONSE_LIMIT as u64) {
            return Err("Audible response too large".into());
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            if bytes.len() + chunk.len() > RESPONSE_LIMIT {
                return Err("Audible response too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let bytes = Arc::new(bytes);
        // Website challenge pages must never poison the success cache.
        let lower = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        if lower.contains("captcha") || lower.contains("<title>robot check") {
            return Err("Audible website challenge; retry later".into());
        }
        let mut cache = self.cache.lock().await;
        if cache.len() >= 256 || cache.values().map(|(_, bytes)| bytes.len()).sum::<usize>() + bytes.len() > 16 * 1024 * 1024 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), bytes.clone()));
        Ok(bytes)
    }

    async fn json(&self, origin: &str, path: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let mut url = reqwest::Url::parse(&format!("{origin}{path}")).map_err(|e| e.to_string())?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        serde_json::from_slice(&self.get(url).await?).map_err(|e| format!("Invalid Audible response: {e}"))
    }

    async fn product(&self, region: &str, origin: &str, asin: &str, source: DiscoverySource) -> Result<Candidate, String> {
        if let Ok(candidate) = self.audnexus_product(region, asin, source).await {
            return Ok(candidate);
        }
        let value = self.json(origin, &format!("/1.0/catalog/products/{asin}"), &[("response_groups", GROUPS)]).await?;
        parse_product(value.get("product").ok_or("Audible product missing")?, region, source)
    }

    async fn chapters(&self, origin: &str, candidate: &mut Candidate) -> Result<(), String> {
        if let Ok((chapters, duration)) = self.audnexus_chapters(candidate).await {
            candidate.chapters = chapters;
            candidate.duration_ms = Some(duration);
            candidate.chapter_provider = Some("audnexus".into());
            return Ok(());
        }
        let value = self.json(origin, &format!("/1.0/content/{}/metadata", candidate.asin), &[("response_groups", "chapter_info"), ("chapter_titles_type", "Tree")]).await?;
        let (chapters, duration) = parse_chapters(&value)?;
        candidate.chapters = chapters;
        candidate.duration_ms = Some(duration);
        candidate.chapter_provider = Some("audible".into());
        Ok(())
    }

    pub(crate) async fn lookup(&self, request: LookupRequest) -> LookupResponse {
        if let Err(detail) = validate(&request) {
            return response(LookupStatus::NoMatch, Vec::new(), false, detail);
        }
        let Ok(_permit) = self.lookups.try_acquire() else {
            return response(LookupStatus::Unavailable, Vec::new(), false, "Audible lookup capacity is busy; retry later".into());
        };
        match tokio::time::timeout(Duration::from_secs(45), self.discover(&request)).await {
            Ok(result) => result,
            Err(_) => response(LookupStatus::Unavailable, Vec::new(), false, "Audible discovery timed out; retry later".into()),
        }
    }

    async fn discover(&self, request: &LookupRequest) -> LookupResponse {
        let mut candidates = Vec::new();
        let mut errors = Vec::new();
        for (region, api, _) in self.origins.iter() {
            for asin in request.recording.asins.iter().take(2) {
                match self.product(region, api, asin, DiscoverySource::EmbeddedAsin).await {
                    Ok(candidate) => candidates.push(candidate),
                    Err(error) => errors.push(error),
                }
            }
        }
        self.hydrate(request, &mut candidates, &mut errors).await;
        if matches!(select_offthread(request, &candidates).await, contract::selection::Selection::Supported(_)) {
            return finish(request, candidates, false, errors).await;
        }
        let queries = queries(request);
        for (region, api, _) in self.origins.iter() {
            for query in &queries {
                match self.json(api, "/1.0/catalog/products", &[(query.0, query.1.as_str()), ("num_results", "10"), ("products_sort_by", "Relevance"), ("response_groups", GROUPS)]).await {
                    Ok(value) => {
                        if let Some(products) = value.get("products").and_then(Value::as_array) {
                            for product in products.iter().take(10) {
                                if let Ok(candidate) = parse_product(product, region, DiscoverySource::Api) {
                                    push_candidate(request, &mut candidates, candidate);
                                }
                            }
                        } else {
                            errors.push("Audible search omitted products".into());
                        }
                    }
                    Err(error) => errors.push(error),
                }
            }
        }
        self.hydrate(request, &mut candidates, &mut errors).await;
        if matches!(select_offthread(request, &candidates).await, contract::selection::Selection::Supported(_)) {
            return finish(request, candidates, false, errors).await;
        }
        // A nonempty API response does not suppress fallback when its candidates
        // are unsuitable or ambiguous for this recording.
        for (region, api, web) in self.origins.iter() {
            for query in &website_queries(request) {
                let mut url = reqwest::Url::parse(&format!("{web}/search")).expect("configured origin");
                url.query_pairs_mut().append_pair("keywords", &query.1).append_pair("ipRedirectOverride", "true");
                match self.get(url).await {
                    Ok(html) => match website_asins(&String::from_utf8_lossy(&html)) {
                        Ok(asins) => {
                            for asin in asins.into_iter().take(8) {
                                if candidates.iter().any(|c| c.region == *region && c.asin == asin) {
                                    continue;
                                }
                                match self.product(region, api, &asin, DiscoverySource::Website).await {
                                    Ok(candidate) => push_candidate(request, &mut candidates, candidate),
                                    Err(error) => errors.push(error),
                                }
                            }
                        }
                        Err(error) => errors.push(error),
                    },
                    Err(error) => errors.push(error),
                }
            }
        }
        self.hydrate(request, &mut candidates, &mut errors).await;
        finish(request, candidates, true, errors).await
    }

    async fn hydrate(&self, request: &LookupRequest, candidates: &mut [Candidate], errors: &mut Vec<String>) {
        for candidate in candidates {
            if candidate.duration_ms.is_some() || contract::selection::title_score(request, candidate) < 950 {
                continue;
            }
            let Some((_, origin, _)) = self.origins.iter().find(|(region, _, _)| *region == candidate.region) else {
                continue;
            };
            if candidate.metadata_provider != "audnexus" {
                if let Ok(enriched) = self.audnexus_product(&candidate.region, &candidate.asin, candidate.source).await {
                    // Replace the candidate as a whole, then revalidate its title.
                    // Never attach another recording's timing to Audible metadata.
                    if contract::selection::title_score(request, &enriched) >= 950 {
                        *candidate = enriched;
                    }
                }
            }
            if let Err(error) = self.chapters(origin, candidate).await {
                errors.push(error);
            }
        }
    }
}

fn push_candidate(request: &LookupRequest, candidates: &mut Vec<Candidate>, candidate: Candidate) {
    if (candidates.len() < CANDIDATE_LIMIT || (candidate.source == DiscoverySource::Website && candidates.len() < 2 * CANDIDATE_LIMIT))
        && contract::selection::title_score(request, &candidate) >= 850
        && !candidates.iter().any(|c| c.region == candidate.region && c.asin == candidate.asin)
    {
        candidates.push(candidate);
    }
}

fn queries(request: &LookupRequest) -> Vec<(&'static str, String)> {
    let full = match request.recording.subtitle.as_deref().filter(|s| !request.title.contains(*s)) {
        Some(subtitle) => format!("{}: {subtitle}", request.title),
        None => request.title.clone(),
    };
    let author = request.authors.iter().flat_map(|name| metadata_contract::matching::author_match_tokens(name)).filter(|token| !matches!(token.as_str(), "dr" | "md" | "phd" | "prof")).collect::<Vec<_>>().join(" ");
    let mut queries = narrator_queries(request, &full, &author);
    queries.push(("title", full.clone()));
    if let Some((_, title)) = contract::selection::split_title_search_hint(&full) {
        queries.push(("title", title));
    }
    if !author.is_empty() {
        queries.push(("keywords", format!("{full} {author}")));
    }
    let primary = full.split(':').next().unwrap_or(&full).trim().to_owned();
    if primary != full {
        queries.push(("title", primary));
    }
    normalize_queries(queries)
}

// Narrator tags are discovery hints, never proof of a recording match. Keep
// broader queries as fallbacks for incomplete or inaccurate contributor tags.
fn narrator_queries(request: &LookupRequest, full: &str, author: &str) -> Vec<(&'static str, String)> {
    let narrators =
        request.recording.narrators.iter().map(|name| name.trim()).filter(|name| !name.is_empty() && name.len() <= 256 && !name.contains("www.") && !name.contains("://") && !name.contains('@')).take(2).collect::<Vec<_>>().join(" ");
    if narrators.is_empty() {
        return Vec::new();
    }
    let primary = full.split(':').next().unwrap_or(full).trim();
    let mut queries = vec![("keywords", format!("{primary} {author} {narrators}"))];
    if primary != full {
        queries.push(("keywords", format!("{full} {author} {narrators}")));
    }
    queries
}

fn normalize_queries(queries: Vec<(&'static str, String)>) -> Vec<(&'static str, String)> {
    let mut result = Vec::new();
    for (field, query) in queries {
        let query = (field, normalize_search_query(&query));
        if !result.contains(&query) {
            result.push(query);
        }
    }
    result
}

fn website_queries(request: &LookupRequest) -> Vec<(&'static str, String)> {
    // The website only receives keywords, so title/keywords variants that
    // normalize to the same text must not consume the lookup budget twice.
    normalize_queries(queries(request).into_iter().map(|(_, query)| ("keywords", query)).collect())
}

fn normalize_search_query(value: &str) -> String {
    value.replace(['_', '-', '–', '—'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate(request: &LookupRequest) -> Result<(), String> {
    if request.title.trim().is_empty()
        || request.title.len() > 4096
        || request.authors.len() > 32
        || request.chapters.len() > 10_000
        || request.recording.asins.len() > 8
        || request.duration_ms == 0
        || request.duration_ms > 365 * 24 * 3600 * 1000
    {
        return Err("Invalid audiobook lookup evidence".into());
    }
    if request.recording.asins.iter().any(|asin| !parsing::valid_asin(asin)) {
        return Err("Invalid embedded ASIN".into());
    }
    Ok(())
}

fn response(status: LookupStatus, candidates: Vec<Candidate>, fallback: bool, detail: String) -> LookupResponse {
    LookupResponse { status, candidates, selected_asin: None, selected_region: None, chapter_plan: None, used_website_fallback: fallback, detail }
}

async fn finish(request: &LookupRequest, candidates: Vec<Candidate>, fallback: bool, errors: Vec<String>) -> LookupResponse {
    let selection = select_offthread(request, &candidates).await;
    let (status, selected, detail) = match selection {
        contract::selection::Selection::Supported(index) => (LookupStatus::Supported, Some(index), "Recording supported by title and recording evidence".to_owned()),
        contract::selection::Selection::Unavailable => (LookupStatus::Unavailable, None, "A plausible recording still needs its chapter lookup; retry later".to_owned()),
        contract::selection::Selection::Ambiguous => (LookupStatus::Ambiguous, None, "Several recording candidates remain plausible".to_owned()),
        contract::selection::Selection::NoMatch if !errors.is_empty() => (LookupStatus::Unavailable, None, errors.join("; ").chars().take(1024).collect()),
        _ => (LookupStatus::NoMatch, None, "No suitable Audible recording found".to_owned()),
    };
    let mut result = response(status, candidates, fallback, detail);
    if let Some(index) = selected {
        result.selected_asin = Some(result.candidates[index].asin.clone());
        result.selected_region = Some(result.candidates[index].region.clone());
        result.detail = format!("Recording supported by title and recording evidence; metadata: {}; chapters: {}", result.candidates[index].metadata_provider, result.candidates[index].chapter_provider.as_deref().unwrap_or("unknown"));
        let request = request.clone();
        let candidate = result.candidates[index].clone();
        result.chapter_plan = tokio::task::spawn_blocking(move || contract::chapters::plan(&request, &candidate)).await.ok().flatten();
    }
    for (index, candidate) in result.candidates.iter_mut().enumerate() {
        if Some(index) != selected {
            candidate.chapters.clear();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_normalization_separates_words_without_guessing_author_prefixes() {
        assert_eq!(normalize_search_query("Paul_Bushkovitch - A Concise History of Russia"), "Paul Bushkovitch A Concise History of Russia");
        assert_eq!(normalize_search_query("Post-war—Europe:  A_History"), "Post war Europe: A History");
        assert_eq!(normalize_search_query("Albion's Seed"), "Albion's Seed");
    }

    #[test]
    fn filename_title_adds_a_discovery_query_without_rewriting_authors() {
        let mut r = request();
        r.title = "Paul_Bushkovitch_-_A_Concise_History_of_Russia".into();
        r.authors.clear();
        let searches = queries(&r);
        assert!(searches.contains(&("title", "A Concise History of Russia".into())));
        assert!(searches.contains(&("title", "Paul Bushkovitch A Concise History of Russia".into())));
        assert!(r.authors.is_empty());
    }
    #[test]
    fn narrator_search_prioritizes_primary_title_and_retains_broad_fallbacks() {
        let mut r = request();
        r.recording.subtitle = Some("A richer subtitle".into());
        r.recording.narrators = vec!["Narrator_One".into()];
        let plan = queries(&r);
        assert_eq!(plan[0], ("keywords", "A Specific Book john smith Narrator One".into()));
        assert!(plan.contains(&("title", "A Specific Book".into())));
        assert!(plan.contains(&("title", "A Specific Book: A richer subtitle".into())));
        assert!(website_queries(&r)[0].1.contains("Narrator One"));
        assert_eq!(r.recording.narrators, vec!["Narrator_One"]);
    }

    #[test]
    fn website_credit_is_not_searched_as_a_narrator() {
        let mut r = request();
        r.recording.narrators = vec!["www.MyAnonamouse.Net".into(), "https://example.org".into()];
        assert!(!queries(&r).iter().any(|(_, q)| q.contains("www.") || q.contains("example.org")));
        assert_eq!(queries(&r)[0].0, "title");
    }

    use axum::{extract::Path, response::IntoResponse, routing::get, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request() -> LookupRequest {
        LookupRequest {
            title: "A Specific Book".into(),
            authors: vec!["John Q. Smith".into()],
            duration_ms: 600_000,
            recording: RecordingEvidence::default(),
            chapters: vec![Chapter { title: "Full audiobook".into(), start_ms: 0, end_ms: 600_000 }],
        }
    }
    fn product(asin: &str) -> Value {
        json!({"asin":asin,"title":"A Specific Book","authors":[{"name":"John Smith"}],"narrators":[{"name":"Narrator One"}],
            "runtime_length_min":99,"isbn":"9780306406157"})
    }
    async fn fixture(api_correct: bool, unavailable: bool) -> (AudibleClient, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let websites = Arc::new(AtomicUsize::new(0));
        let count = websites.clone();
        let router = Router::new()
            .route(
                "/1.0/catalog/products",
                get(move || async move {
                    if unavailable {
                        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({})));
                    }
                    let asin = if api_correct { "B000000002" } else { "B000000001" };
                    (axum::http::StatusCode::OK, axum::Json(json!({"products":[product(asin)]})))
                }),
            )
            .route("/1.0/catalog/products/:asin", get(|Path(asin): Path<String>| async move { axum::Json(json!({"product":product(&asin)})) }))
            .route(
                "/1.0/content/:asin/metadata",
                get(|Path(asin): Path<String>| async move {
                    let length = if asin == "B000000002" { 300_000 } else { 600_000 };
                    axum::Json(json!({"content_metadata":{"chapter_info":{"chapters":[
                        {"title":"Origins of the project","start_offset_ms":0,"length_ms":length},
                        {"title":"The turning point","start_offset_ms":length,"length_ms":length}
                    ]}}}))
                }),
            )
            .route(
                "/search",
                get(move |axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>| {
                    assert_eq!(query.get("ipRedirectOverride").map(String::as_str), Some("true"), "website search must preserve the selected region");
                    count.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if unavailable {
                            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response();
                        }
                        "<html><title>Search</title><a href='/pd/Book/B000000002'>A Specific Book</a></html>".into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut client = AudibleClient::new();
        client.audnexus_origin = origin.clone();
        client.origins = Arc::new(vec![("us".into(), origin.clone(), origin)]);
        (client, websites, task)
    }

    #[tokio::test]
    async fn unsuitable_api_hit_uses_website_and_precise_runtime_then_reuses_cache() {
        let (client, websites, server) = fixture(false, false).await;
        let result = client.lookup(request()).await;
        assert_eq!(result.status, LookupStatus::Supported, "{}", result.detail);
        assert_eq!(result.selected_asin.as_deref(), Some("B000000002"));
        assert!(result.used_website_fallback);
        assert!(websites.load(Ordering::SeqCst) > 0);
        let before = websites.load(Ordering::SeqCst);
        assert_eq!(client.lookup(request()).await.status, LookupStatus::Supported);
        assert_eq!(websites.load(Ordering::SeqCst), before, "cached discovery must not repeat website requests");
        server.abort();
    }

    #[tokio::test]
    async fn narrator_website_query_discovers_recording_missing_from_api() {
        let (mut client, _, api_server) = fixture(false, false).await;
        let hits = Arc::new(AtomicUsize::new(0));
        let count = hits.clone();
        let router = Router::new().route(
            "/search",
            get(move |axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>| {
                let matched = query.get("keywords").is_some_and(|q| q == "A Specific Book john smith Narrator One");
                if matched {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                async move {
                    if matched {
                        "<html><title>Search</title><a href='/pd/Book/B000000002'>A Specific Book</a></html>"
                    } else {
                        "<html><title>Search</title>No results</html>"
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let web = format!("http://{}", listener.local_addr().unwrap());
        let web_server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let (region, api, _) = client.origins[0].clone();
        client.origins = Arc::new(vec![(region, api, web)]);
        let mut r = request();
        r.recording.narrators = vec!["Narrator One".into()];
        let result = client.lookup(r).await;
        assert_eq!(result.status, LookupStatus::Supported, "{}", result.detail);
        assert_eq!(result.selected_asin.as_deref(), Some("B000000002"));
        assert!(result.used_website_fallback);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        api_server.abort();
        web_server.abort();
    }

    #[tokio::test]
    async fn suitable_api_result_does_not_use_website() {
        let (client, websites, server) = fixture(true, false).await;
        let result = client.lookup(request()).await;
        assert_eq!(result.status, LookupStatus::Supported, "{}", result.detail);
        assert!(!result.used_website_fallback);
        assert_eq!(websites.load(Ordering::SeqCst), 0);
        assert_eq!(result.candidates[0].chapter_provider.as_deref(), Some("audible"));
        server.abort();
    }

    #[tokio::test]
    async fn audnexus_supplies_recording_metadata_and_chapters_with_audible_discovery() {
        let (mut client, websites, server) = fixture(true, false).await;
        let router = Router::new()
            .route(
                "/books/:asin",
                get(|Path(asin): Path<String>| async move {
                    axum::Json(json!({"asin":asin,"region":"us","title":"A Specific Book",
                    "authors":[{"name":"John Smith"}],"publisherName":"Audnexus publisher"}))
                }),
            )
            .route(
                "/books/:asin/chapters",
                get(|Path(asin): Path<String>| async move {
                    axum::Json(json!({"asin":asin,"region":"us","isAccurate":true,"runtimeLengthMs":600000,
                    "chapters":[{"title":"Origins of the project","startOffsetMs":0,"lengthMs":300000},
                        {"title":"The turning point","startOffsetMs":300000,"lengthMs":300000}]}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        client.audnexus_origin = format!("http://{}", listener.local_addr().unwrap());
        let audnexus = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut request = request();
        request.chapters = vec![Chapter { title: "Chapter 1".into(), start_ms: 0, end_ms: 300000 }, Chapter { title: "Chapter 2".into(), start_ms: 300000, end_ms: 600000 }];
        let result = client.lookup(request).await;
        assert_eq!(result.status, LookupStatus::Supported, "{}", result.detail);
        let candidate = &result.candidates[0];
        assert_eq!(candidate.metadata_provider, "audnexus");
        assert_eq!(candidate.chapter_provider.as_deref(), Some("audnexus"));
        assert_eq!(candidate.publisher.as_deref(), Some("Audnexus publisher"));
        assert!(result.chapter_plan.is_some());
        assert_eq!(websites.load(Ordering::SeqCst), 0);
        audnexus.abort();
        server.abort();
    }

    #[tokio::test]
    async fn unavailable_providers_are_retryable_and_capacity_does_not_queue() {
        let (client, _, server) = fixture(false, true).await;
        assert_eq!(client.lookup(request()).await.status, LookupStatus::Unavailable);
        let _permits = client.lookups.acquire_many(2).await.unwrap();
        let result = client.lookup(request()).await;
        assert_eq!(result.status, LookupStatus::Unavailable);
        assert!(result.detail.contains("capacity"));
        server.abort();
    }

    #[test]
    fn subtitle_and_author_initial_normalization_are_used_in_search() {
        let mut r = request();
        r.recording.subtitle = Some("A richer subtitle".into());
        let plan = queries(&r);
        assert_eq!(plan[0], ("title", "A Specific Book: A richer subtitle".into()));
        assert!(plan.iter().any(|(field, value)| *field == "keywords" && value.contains("john smith")));
        assert!(!plan.iter().any(|(_, value)| value.contains(" Q.")));
    }
}

async fn select_offthread(request: &LookupRequest, candidates: &[Candidate]) -> contract::selection::Selection {
    let request = request.clone();
    let candidates = candidates.to_vec();
    tokio::task::spawn_blocking(move || contract::selection::select(&request, &candidates)).await.unwrap_or(contract::selection::Selection::Ambiguous)
}

#[cfg(test)]
mod unresolved_candidate_regressions {
    use super::*;
    #[tokio::test]
    async fn failed_competing_chapter_lookup_never_returns_a_write_plan() {
        let request = LookupRequest {
            title: "A Specific Book".into(),
            authors: vec!["John Smith".into()],
            duration_ms: 600_000,
            recording: RecordingEvidence::default(),
            chapters: vec![Chapter { title: "Full audiobook".into(), start_ms: 0, end_ms: 600_000 }],
        };
        let product = serde_json::json!({"asin":"B000000001","title":"A Specific Book","authors":[{"name":"John Smith"}]});
        let missing = parse_product(&product, "us", DiscoverySource::Api).unwrap();
        let mut complete = missing.clone();
        complete.asin = "B000000002".into();
        complete.duration_ms = Some(600_000);
        complete.chapters = vec![Chapter { title: "A descriptive chapter".into(), start_ms: 0, end_ms: 600_000 }];
        let result = finish(&request, vec![missing, complete], true, vec!["Audible HTTP 503".into()]).await;
        assert_eq!(result.status, LookupStatus::Unavailable);
        assert!(result.selected_asin.is_none());
        assert!(result.chapter_plan.is_none());
        assert_eq!(result.candidates.len(), 2);
    }
}
