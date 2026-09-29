use super::{
    error, fs, get, header, middleware, post, query_connection, BTreeMap, Bytes, DefaultBodyLimit, Duration, EndpointMetrics, EndpointMetricsSnapshot, HeaderMap, HeaderValue, Instant, IntoResponse, MetadataError, MetadataService, Next,
    OperationalStats, Ordering, Request, Response, Router, ServiceState, State, StatusCode, MAX_ISBNS_PER_REQUEST, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, QUERY_CONCURRENCY,
};
use crate::query::run_database;

pub fn app(service: MetadataService) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/health/descriptions", get(description_capability))
        .route("/internal/operations", get(operational_stats))
        .route("/internal/librarything", get(librarything_stats))
        .route("/v2/classifications", post(rich_classifications))
        .route("/v2/enrichment", post(rich_enrichment))
        .route("/v2/edition-identities", post(edition_identities))
        .route("/v2/audiobooks/audible", post(audible_lookup).get(audible_cover).layer::<_, std::convert::Infallible>(DefaultBodyLimit::max(metadata_contract::audible::MAX_LOOKUP_REQUEST_BYTES)).layer(audiobook_cors()))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(service.clone(), record_request_metrics))
        .with_state(service)
}

async fn librarything_stats(State(service): State<MetadataService>) -> Response {
    let Some(client) = service.librarything.clone() else {
        return axum::Json(serde_json::json!({"enabled":false})).into_response();
    };
    match tokio::task::spawn_blocking(move || crate::librarything_background::stats(&client)).await {
        Ok(Ok(stats)) => axum::Json(stats).into_response(),
        Ok(Err(e)) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn description_capability(State(service): State<MetadataService>) -> (StatusCode, &'static str) {
    if service.state.descriptions_available {
        (StatusCode::OK, "available")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "snapshot has no descriptions")
    }
}

async fn record_request_metrics(State(service): State<MetadataService>, request: Request, next: Next) -> Response {
    let endpoint = match request.uri().path() {
        "/v2/classifications" => Some(&service.state.classifications),
        "/v2/enrichment" => Some(&service.state.enrichment),
        "/v2/edition-identities" => Some(&service.state.edition_identities),
        _ => None,
    };
    let started = Instant::now();
    if let Some(endpoint) = endpoint {
        endpoint.in_flight.fetch_add(1, Ordering::Relaxed);
    }
    let response = next.run(request).await;
    if let Some(endpoint) = endpoint {
        endpoint.in_flight.fetch_sub(1, Ordering::Relaxed);
        endpoint.requests.fetch_add(1, Ordering::Relaxed);
        if !response.status().is_success() {
            endpoint.errors.fetch_add(1, Ordering::Relaxed);
        }
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        endpoint.total_duration_ms.fetch_add(duration_ms, Ordering::Relaxed);
        service.state.requests.fetch_add(1, Ordering::Relaxed);
        if !response.status().is_success() {
            service.state.errors.fetch_add(1, Ordering::Relaxed);
        }
        service.state.total_duration_ms.fetch_add(duration_ms, Ordering::Relaxed);
    }
    response
}

async fn operational_stats(State(service): State<MetadataService>) -> Response {
    match run_database(service, |service| load_operational_stats(&service.state)).await {
        Ok(Ok(stats)) => {
            let mut response = axum::Json(stats).into_response();
            response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            response
        }
        Ok(Err(error)) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("statistics worker failed: {error}")),
    }
}

pub(crate) fn load_operational_stats(state: &ServiceState) -> Result<OperationalStats, MetadataError> {
    let statistics_pool = state.pool.as_ref().unwrap_or(&state.rich_pool);
    let connection = query_connection(statistics_pool)?;
    let (edition_records, work_records, author_records): (i64, i64, i64) =
        connection.query_row("SELECT edition_records,work_records,author_records FROM snapshot WHERE singleton=1", (), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(error)?;
    let mut metrics = BTreeMap::new();
    if let Ok(mut statement) = connection.prepare("SELECT name,value FROM metadata_metric ORDER BY name") {
        if let Ok(rows) = statement.query_map((), |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))) {
            for row in rows.flatten() {
                metrics.insert(row.0, row.1);
            }
        }
    }
    let requests = state.requests.load(Ordering::Relaxed);
    let total_duration_ms = state.total_duration_ms.load(Ordering::Relaxed);
    let processed_items = state.processed_items.load(Ordering::Relaxed);
    let item_processing_duration_ms = state.item_processing_duration_ms.load(Ordering::Relaxed);
    let pool_state = state.pool.as_ref().map_or_else(|| state.rich_pool.state(), |pool| pool.state());
    let endpoint_metrics = [("edition_identities", endpoint_metrics_snapshot(&state.edition_identities)), ("classifications", endpoint_metrics_snapshot(&state.classifications)), ("enrichment", endpoint_metrics_snapshot(&state.enrichment))]
        .into_iter()
        .collect();
    Ok(OperationalStats {
        status: "available",
        uptime_seconds: state.started_at.elapsed().as_secs(),
        schema_version: state.schema_version,
        dump_date: state.snapshot.dump_date.clone(),
        imported_at_ms: state.snapshot.imported_at_ms,
        database_bytes: fs::metadata(&state.database).map_err(error)?.len(),
        edition_records: u64::try_from(edition_records).unwrap_or_default(),
        work_records: u64::try_from(work_records).unwrap_or_default(),
        author_records: u64::try_from(author_records).unwrap_or_default(),
        requests,
        errors: state.errors.load(Ordering::Relaxed),
        average_duration_ms: if requests == 0 { 0.0 } else { total_duration_ms as f64 / requests as f64 },
        processed_items,
        average_duration_ms_per_item: if processed_items == 0 { 0.0 } else { item_processing_duration_ms as f64 / processed_items as f64 },
        items_per_processing_second: if item_processing_duration_ms == 0 { 0.0 } else { processed_items as f64 * 1_000.0 / item_processing_duration_ms as f64 },
        query_concurrency: QUERY_CONCURRENCY,
        pool_connections: pool_state.connections,
        pool_idle_connections: pool_state.idle_connections,
        metrics,
        endpoint_metrics,
    })
}

fn endpoint_metrics_snapshot(metrics: &EndpointMetrics) -> EndpointMetricsSnapshot {
    let requests = metrics.requests.load(Ordering::Relaxed);
    let total_duration_ms = metrics.total_duration_ms.load(Ordering::Relaxed);
    EndpointMetricsSnapshot {
        requests,
        errors: metrics.errors.load(Ordering::Relaxed),
        in_flight: metrics.in_flight.load(Ordering::Relaxed),
        total_duration_ms,
        average_duration_ms: if requests == 0 { 0.0 } else { total_duration_ms as f64 / requests as f64 },
    }
}

async fn edition_identities(State(service): State<MetadataService>, headers: HeaderMap, body: Bytes) -> Response {
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or_default();
    let json = content_type.split(';').next().is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    let binary = metadata_contract::is_binary_media_type_v2(content_type);
    let decoded = match (json, binary) {
        (true, _) => serde_json::from_slice(&body).map_err(|error| MetadataError(format!("invalid JSON request: {error}"))),
        (_, true) => metadata_contract::decode_v2_edition_identity_request(&body, MAX_REQUEST_BYTES).map_err(|error| MetadataError(error.to_string())),
        _ => return api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "use application/json or application/vnd.bokheim.metadata+protobuf; version=2"),
    };
    let request = match decoded {
        Ok(request) => request,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let item_count = u64::try_from(request.queries.len()).unwrap_or(u64::MAX);
    let metrics = service.state.clone();
    let processing_started = Instant::now();
    let result = match run_database(service, move |service| service.resolve_editions(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
        Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("lookup worker failed: {error}")),
    };
    record_processed_items(&metrics, item_count, processing_started.elapsed());
    let encoded = if binary { metadata_contract::encode_v2_edition_identity_response(&result) } else { serde_json::to_vec(&result).map_err(|error| metadata_contract::WireError::Encode(error.to_string())) };
    match encoded {
        Ok(encoded) if encoded.len() <= MAX_RESPONSE_BYTES => {
            let mut response = encoded.into_response();
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(if binary { metadata_contract::MEDIA_TYPE_V2 } else { "application/json" }));
            response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
            response
        }
        Ok(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "encoded response exceeded the response limit"),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) fn validate_isbn_count(count: usize) -> Result<(), MetadataError> {
    if count == 0 {
        return Err(MetadataError("at least one ISBN is required".to_owned()));
    }
    if count > MAX_ISBNS_PER_REQUEST {
        return Err(MetadataError(format!("at most {MAX_ISBNS_PER_REQUEST} ISBNs may be requested")));
    }
    Ok(())
}

async fn rich_classifications(State(service): State<MetadataService>, headers: HeaderMap, body: Bytes) -> Response {
    let binary = headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(metadata_contract::is_binary_media_type_v2);
    let request = if binary {
        metadata_contract::decode_v2_request(&body, MAX_REQUEST_BYTES).map_err(|error| MetadataError(error.to_string()))
    } else if headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.split(';').next().is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))) {
        serde_json::from_slice(&body).map_err(|error| MetadataError(format!("invalid JSON request: {error}")))
    } else {
        return api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "use application/json or application/vnd.bokheim.metadata+protobuf; version=2");
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let item_count = u64::try_from(request.isbns.len()).unwrap_or(u64::MAX);
    let metrics = service.state.clone();
    let processing_started = Instant::now();
    let fallback = service.clone();
    let mut result = match run_database(service, move |service| service.classify_rich(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
        Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("lookup worker failed: {error}")),
    };
    fallback.apply_library_of_congress_fallback(&mut result).await;
    fallback.apply_librarything_fallback(&mut result).await;
    record_processed_items(&metrics, item_count, processing_started.elapsed());
    let (content_type, encoded) = if binary {
        match metadata_contract::encode_v2_response(&result) {
            Ok(encoded) => (metadata_contract::MEDIA_TYPE_V2, encoded),
            Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        }
    } else {
        match serde_json::to_vec(&result) {
            Ok(encoded) => ("application/json", encoded),
            Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        }
    };
    if encoded.len() > MAX_RESPONSE_BYTES {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "encoded response exceeded the response limit");
    }
    let mut response = encoded.into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
    response
}

async fn rich_enrichment(State(service): State<MetadataService>, headers: HeaderMap, body: Bytes) -> Response {
    let binary = headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(metadata_contract::is_binary_media_type_v2);
    let request = if binary {
        metadata_contract::decode_v2_request(&body, MAX_REQUEST_BYTES).map_err(|error| MetadataError(error.to_string()))
    } else if headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.split(';').next().is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))) {
        serde_json::from_slice(&body).map_err(|error| MetadataError(format!("invalid JSON request: {error}")))
    } else {
        return api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "use application/json or application/vnd.bokheim.metadata+protobuf; version=2");
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let item_count = u64::try_from(request.isbns.len()).unwrap_or(u64::MAX);
    let metrics = service.state.clone();
    let processing_started = Instant::now();
    let fallback = service.clone();
    let mut result = match run_database(service, move |service| service.enrich_rich(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
        Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("lookup worker failed: {error}")),
    };
    fallback.apply_library_of_congress_fallback(&mut result).await;
    fallback.apply_librarything_fallback(&mut result).await;
    record_processed_items(&metrics, item_count, processing_started.elapsed());
    let (content_type, encoded) = if binary {
        match metadata_contract::encode_v2_response(&result) {
            Ok(encoded) => (metadata_contract::MEDIA_TYPE_V2, encoded),
            Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        }
    } else {
        match serde_json::to_vec(&result) {
            Ok(encoded) => ("application/json", encoded),
            Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        }
    };
    if encoded.len() > MAX_RESPONSE_BYTES {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "encoded response exceeded the response limit");
    }
    let mut response = encoded.into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
    response
}

pub(crate) fn record_processed_items(state: &ServiceState, count: u64, duration: Duration) {
    state.processed_items.fetch_add(count, Ordering::Relaxed);
    state.item_processing_duration_ms.fetch_add(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX), Ordering::Relaxed);
}

pub(crate) fn api_error(status: StatusCode, message: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], message.to_owned()).into_response()
}

#[derive(serde::Deserialize)]
struct AudibleCoverQuery {
    region: String,
    asin: String,
}

async fn audible_cover(axum::extract::Query(AudibleCoverQuery { region, asin }): axum::extract::Query<AudibleCoverQuery>) -> Response {
    static CLIENT: std::sync::OnceLock<crate::audible::AudibleClient> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(crate::audible::AudibleClient::new);
    match tokio::time::timeout(Duration::from_secs(30), client.cover(&region, &asin)).await {
        Ok(Ok(Some(bytes))) => ([(header::CONTENT_TYPE, "application/octet-stream"), (header::CACHE_CONTROL, "public, max-age=21600")], bytes).into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(error)) => api_error(StatusCode::BAD_GATEWAY, &error),
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

async fn audible_lookup(axum::Json(request): axum::Json<metadata_contract::audible::LookupRequest>) -> axum::Json<metadata_contract::audible::LookupResponse> {
    static CLIENT: std::sync::OnceLock<crate::audible::AudibleClient> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(crate::audible::AudibleClient::new);
    let response = client.lookup(request).await;
    tracing::info!(status = ?response.status, asin = ?response.selected_asin, region = ?response.selected_region,
        detail = %response.detail, "audiobook enrichment completed");
    axum::Json(response)
}

fn audiobook_cors() -> tower_http::cors::CorsLayer {
    // Public bibliographic lookups carry no cookies or account credentials.
    tower_http::cors::CorsLayer::new().allow_origin(tower_http::cors::Any).allow_methods([axum::http::Method::POST, axum::http::Method::GET]).allow_headers([header::CONTENT_TYPE])
}

#[cfg(test)]
mod audiobook_http_tests {
    use super::*;

    #[tokio::test]
    async fn browser_preflight_and_invalid_request_reach_the_audiobook_route() {
        let router = Router::new().route("/v2/audiobooks/audible", post(audible_lookup).get(audible_cover).layer(audiobook_cors()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v2/audiobooks/audible", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::new();
        let preflight =
            client.request(reqwest::Method::OPTIONS, &url).header("Origin", "https://app.bokheim.se").header("Access-Control-Request-Method", "POST").header("Access-Control-Request-Headers", "content-type").send().await.unwrap();
        assert!(preflight.status().is_success());
        assert_eq!(preflight.headers()["access-control-allow-origin"], "*");
        assert_eq!(preflight.headers()["access-control-allow-methods"], "POST,GET");
        let invalid = client.post(&url).header("Origin", "https://app.bokheim.se").header("Content-Type", "application/json").body("{}").send().await.unwrap();
        assert_eq!(invalid.status().as_u16(), 422);
        assert_eq!(invalid.headers()["access-control-allow-origin"], "*");
        task.abort();
    }
}
