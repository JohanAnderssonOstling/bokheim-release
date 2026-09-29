use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path as FilePath, PathBuf};
use std::sync::Arc;
use subject_projection::{lcc_subject_sort_orders, published_taxonomy_release_id, read_unified_taxonomy_sqlite, UnifiedConceptDefinition, UnifiedTaxonomy, BISAC_SYSTEM_ID, DDC_SYSTEM_ID, LCC_SYSTEM_ID};
use taxonomy_contract::{
    TaxonomyBatchRequest, TaxonomyBatchResponse, TaxonomyCodeResolution, TaxonomyConcept, TaxonomyDescription, TaxonomyParent, TaxonomyResolution, TaxonomySlice, TaxonomySystem, API_VERSION, MAX_CODES_PER_REQUEST, MAX_CODE_BYTES,
    MAX_REQUEST_BYTES, SNAPSHOT_MEDIA_TYPE,
};
use tower_http::cors::{Any, CorsLayer};

const JSON_CACHE_CONTROL: &str = "public, max-age=3600";
const SNAPSHOT_CACHE_CONTROL: &str = "public, max-age=31536000, immutable, no-transform";

#[derive(Clone)]
pub struct TaxonomyService {
    state: Arc<ServiceState>,
}

struct ServiceState {
    matcher: UnifiedTaxonomy,
    description: TaxonomyDescription,
    concepts: BTreeMap<i64, TaxonomyConcept>,
    children_by_parent: BTreeMap<i64, Vec<i64>>,
    roots: Vec<i64>,
    snapshot: Bytes,
    etag: HeaderValue,
}

#[derive(Debug)]
pub struct TaxonomyError(String);

impl fmt::Display for TaxonomyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TaxonomyError {}

#[derive(Deserialize)]
struct ResolveQuery {
    system: TaxonomySystem,
    code: String,
}

impl TaxonomyService {
    pub fn open(database: impl Into<PathBuf>) -> Result<Self, TaxonomyError> {
        let database = database.into();
        let release_id = published_taxonomy_release_id(&database).map_err(TaxonomyError)?;
        let definitions = read_unified_taxonomy_sqlite(&database).map_err(TaxonomyError)?;
        let snapshot = Bytes::from(fs::read(&database).map_err(|error| TaxonomyError(format!("failed to read taxonomy snapshot {}: {error}", database.display())))?);
        Self::from_parts(release_id, definitions, snapshot)
    }

    fn from_parts(release_id: u64, definitions: Vec<UnifiedConceptDefinition>, snapshot: Bytes) -> Result<Self, TaxonomyError> {
        let matcher = UnifiedTaxonomy::from_concepts(definitions.clone()).map_err(TaxonomyError)?;
        let etag = HeaderValue::from_str(&format!("\"{release_id}\"")).map_err(|error| TaxonomyError(format!("invalid taxonomy release identifier: {error}")))?;
        let concepts = definitions.iter().map(contract_concept).map(|concept| (concept.concept_id.clone(), concept)).collect::<BTreeMap<_, _>>();
        let sort_orders = lcc_subject_sort_orders(&definitions);
        let mut children = BTreeMap::<i64, Vec<(i64, i64)>>::new();
        for concept in concepts.values() {
            for parent in &concept.parents {
                children.entry(parent.concept_id.clone()).or_default().push((sort_orders[&concept.concept_id], concept.concept_id.clone()));
            }
        }
        let mut children_by_parent = BTreeMap::new();
        for (parent, mut values) in children {
            values.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| concepts[&left.1].preferred_label.to_lowercase().cmp(&concepts[&right.1].preferred_label.to_lowercase())).then_with(|| left.1.cmp(&right.1)));
            children_by_parent.insert(parent, values.into_iter().map(|(_, concept_id)| concept_id).collect());
        }
        let mut roots = concepts.values().filter(|concept| concept.parents.is_empty()).map(|concept| concept.concept_id.clone()).collect::<Vec<_>>();
        roots.sort_by(|left, right| sort_orders[left].cmp(&sort_orders[right]).then_with(|| concepts[left].preferred_label.to_lowercase().cmp(&concepts[right].preferred_label.to_lowercase())).then_with(|| left.cmp(right)));
        let edge_count = concepts.values().map(|concept| concept.parents.len() as u64).sum();
        let selector_count = concepts.values().flat_map(|concept| concept.source_selectors.values()).map(|selectors| selectors.len() as u64).sum();
        let description = TaxonomyDescription {
            api_version: API_VERSION.to_owned(),
            release_id,
            format_version: 1,
            taxonomy_name: "Bokheim Unified Subjects".to_owned(),
            concept_count: concepts.len() as u64,
            edge_count,
            selector_count,
            root_count: roots.len() as u64,
            snapshot_bytes: snapshot.len() as u64,
        };
        Ok(Self { state: Arc::new(ServiceState { matcher, description, concepts, children_by_parent, roots, snapshot, etag }) })
    }

    fn slice(&self, requested: impl IntoIterator<Item = i64>) -> Result<TaxonomySlice, TaxonomyError> {
        let requested = requested.into_iter().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        for concept_id in &requested {
            if !self.state.concepts.contains_key(concept_id) {
                return Err(TaxonomyError(format!("unknown taxonomy concept {concept_id}")));
            }
        }
        let mut included = BTreeSet::new();
        let mut ordered = Vec::new();
        for concept_id in &requested {
            self.add_with_ancestors(concept_id, &mut included, &mut ordered);
        }
        Ok(TaxonomySlice { release_id: self.state.description.release_id.clone(), requested_concept_ids: requested, concepts: ordered.into_iter().map(|concept_id| self.state.concepts[&concept_id].clone()).collect() })
    }

    fn add_with_ancestors(&self, concept_id: &i64, included: &mut BTreeSet<i64>, ordered: &mut Vec<i64>) {
        if included.contains(concept_id) {
            return;
        }
        for parent in &self.state.concepts[concept_id].parents {
            self.add_with_ancestors(&parent.concept_id, included, ordered);
        }
        if included.insert(concept_id.to_owned()) {
            ordered.push(concept_id.to_owned());
        }
    }
}

fn contract_concept(definition: &UnifiedConceptDefinition) -> TaxonomyConcept {
    let parents = definition.parent_ids().iter().map(|concept_id| TaxonomyParent { concept_id: *concept_id }).collect();
    let source_selectors = [BISAC_SYSTEM_ID, DDC_SYSTEM_ID, LCC_SYSTEM_ID]
        .into_iter()
        .filter_map(|system| {
            let selectors = definition.source_selectors(system);
            (!selectors.is_empty()).then(|| (system.to_owned(), selectors.to_vec()))
        })
        .collect();
    TaxonomyConcept { concept_id: definition.concept_id().to_owned(), preferred_label: definition.preferred_label().to_owned(), parents, source_selectors }
}

pub fn app(service: TaxonomyService) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/taxonomy", get(description))
        .route("/v1/roots", get(roots))
        .route("/v1/resolve", get(resolve).post(resolve_batch))
        .route("/v1/concepts/:concept_id", get(concept))
        .route("/v1/concepts/:concept_id/children", get(children))
        .route("/v1/snapshot", get(snapshot))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET, Method::POST, Method::HEAD, Method::OPTIONS]).allow_headers([header::ACCEPT, header::CONTENT_TYPE, header::IF_NONE_MATCH]).expose_headers([
            header::CACHE_CONTROL,
            header::CONTENT_DISPOSITION,
            header::ETAG,
        ]))
        .with_state(service)
}

async fn health() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

async fn description(State(service): State<TaxonomyService>, headers: HeaderMap) -> Response {
    if headers.get(header::IF_NONE_MATCH) == Some(&service.state.etag) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, service.state.etag.clone()), (header::CACHE_CONTROL, HeaderValue::from_static(JSON_CACHE_CONTROL))]).into_response();
    }
    cached_json(&service, &service.state.description)
}

async fn roots(State(service): State<TaxonomyService>) -> Response {
    match service.slice(service.state.roots.clone()) {
        Ok(slice) => cached_json(&service, &slice),
        Err(error) => internal_error(error),
    }
}

async fn resolve(State(service): State<TaxonomyService>, Query(query): Query<ResolveQuery>) -> Response {
    let code = query.code.trim();
    if code.is_empty() || code.len() > MAX_CODE_BYTES {
        return api_error(StatusCode::BAD_REQUEST, "code must contain between 1 and 256 bytes");
    }
    let matched = service.state.matcher.matching_concept_ids(query.system.as_str(), code);
    match service.slice(matched.clone()) {
        Ok(slice) => cached_json(&service, &TaxonomyResolution { release_id: slice.release_id, system: query.system, code: code.to_owned(), matched_concept_ids: matched, concepts: slice.concepts }),
        Err(error) => internal_error(error),
    }
}

async fn resolve_batch(State(service): State<TaxonomyService>, Json(request): Json<TaxonomyBatchRequest>) -> Response {
    if request.codes.is_empty() || request.codes.len() > MAX_CODES_PER_REQUEST {
        return api_error(StatusCode::BAD_REQUEST, "codes must contain between 1 and 512 entries");
    }
    let mut codes = BTreeSet::new();
    for mut requested in request.codes {
        requested.code = requested.code.trim().to_owned();
        if requested.code.is_empty() || requested.code.len() > MAX_CODE_BYTES {
            return api_error(StatusCode::BAD_REQUEST, "every code must contain between 1 and 256 bytes");
        }
        codes.insert(requested);
    }
    let mut matched_union = BTreeSet::new();
    let results = codes
        .into_iter()
        .map(|requested| {
            let matched_concept_ids = service.state.matcher.matching_concept_ids(requested.system.as_str(), &requested.code);
            matched_union.extend(matched_concept_ids.iter().cloned());
            TaxonomyCodeResolution { system: requested.system, code: requested.code, matched_concept_ids }
        })
        .collect::<Vec<_>>();
    match service.slice(matched_union) {
        Ok(slice) => cached_json(&service, &TaxonomyBatchResponse { release_id: slice.release_id, results, concepts: slice.concepts }),
        Err(error) => internal_error(error),
    }
}

async fn concept(State(service): State<TaxonomyService>, Path(concept_id): Path<i64>) -> Response {
    match service.slice([concept_id]) {
        Ok(slice) => cached_json(&service, &slice),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

async fn children(State(service): State<TaxonomyService>, Path(concept_id): Path<i64>) -> Response {
    if !service.state.concepts.contains_key(&concept_id) {
        return api_error(StatusCode::NOT_FOUND, &format!("unknown taxonomy concept {concept_id}"));
    }
    match service.slice(service.state.children_by_parent.get(&concept_id).cloned().unwrap_or_default()) {
        Ok(slice) => cached_json(&service, &slice),
        Err(error) => internal_error(error),
    }
}

async fn snapshot(State(service): State<TaxonomyService>, headers: HeaderMap) -> Response {
    if headers.get(header::IF_NONE_MATCH) == Some(&service.state.etag) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, service.state.etag.clone()), (header::CACHE_CONTROL, HeaderValue::from_static(SNAPSHOT_CACHE_CONTROL))]).into_response();
    }
    let mut response = Body::from(service.state.snapshot.clone()).into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(SNAPSHOT_MEDIA_TYPE));
    response.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=unified-taxonomy-v2.sqlite3"));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(SNAPSHOT_CACHE_CONTROL));
    response.headers_mut().insert(header::ETAG, service.state.etag.clone());
    response
}

fn cached_json<T: serde::Serialize>(service: &TaxonomyService, value: &T) -> Response {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(JSON_CACHE_CONTROL));
    response.headers_mut().insert(header::ETAG, service.state.etag.clone());
    response
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], message.to_owned()).into_response()
}

fn internal_error(error: TaxonomyError) -> Response {
    tracing::error!(%error, "taxonomy request failed");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "taxonomy request failed")
}

pub fn default_database_path() -> &'static FilePath {
    FilePath::new("shared/subject-projection/data/unified-taxonomy-v2.sqlite3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::OnceLock;
    use tower::ServiceExt;

    fn service() -> TaxonomyService {
        static SERVICE: OnceLock<TaxonomyService> = OnceLock::new();
        SERVICE
            .get_or_init(|| {
                let database = FilePath::new(env!("CARGO_MANIFEST_DIR")).join("../../shared/subject-projection/data/unified-taxonomy-v2.sqlite3");
                TaxonomyService::open(database).expect("curated taxonomy opens")
            })
            .clone()
    }

    async fn json<T: serde::de::DeserializeOwned>(response: Response) -> T {
        let bytes = response.into_body().collect().await.expect("response body").to_bytes();
        serde_json::from_slice(&bytes).expect("JSON response")
    }

    #[tokio::test]
    async fn public_endpoints_require_no_authentication() {
        let router = app(service());
        for uri in ["/health", "/v1/taxonomy", "/v1/roots", "/v1/resolve?system=lcc&code=HF5601"] {
            let response = router.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn unsupported_codes_are_resolved_in_one_deduplicated_batch() {
        let request = TaxonomyBatchRequest {
            codes: vec![
                taxonomy_contract::TaxonomyCode { system: TaxonomySystem::Lcc, code: "HF5601".to_owned() },
                taxonomy_contract::TaxonomyCode { system: TaxonomySystem::Bisac, code: "MAT000000".to_owned() },
                taxonomy_contract::TaxonomyCode { system: TaxonomySystem::Lcc, code: " HF5601 ".to_owned() },
            ],
        };
        let response = app(service()).oneshot(Request::builder().method(Method::POST).uri("/v1/resolve").header(header::CONTENT_TYPE, "application/json").body(Body::from(serde_json::to_vec(&request).unwrap())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response: TaxonomyBatchResponse = json(response).await;
        assert_eq!(response.results.len(), 2, "trimmed duplicate codes are coalesced");
        assert!(response.results.iter().all(|result| !result.matched_concept_ids.is_empty()));
        let unique = response.concepts.iter().map(|concept| &concept.concept_id).collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), response.concepts.len(), "the shared concept slice is deduplicated");
        let positions = response.concepts.iter().enumerate().map(|(position, concept)| (concept.concept_id, position)).collect::<BTreeMap<_, _>>();
        for (position, concept) in response.concepts.iter().enumerate() {
            for parent in &concept.parents {
                assert!(positions[&parent.concept_id] < position);
            }
        }
    }

    #[tokio::test]
    async fn resolution_returns_an_ancestor_closed_parent_first_slice() {
        let response = app(service()).oneshot(Request::builder().uri("/v1/resolve?system=lcc&code=HF5601").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let resolution: TaxonomyResolution = json(response).await;
        assert!(!resolution.matched_concept_ids.is_empty());
        assert!(resolution.concepts.iter().any(|concept| concept.preferred_label == "Accounting"));
        let positions = resolution.concepts.iter().enumerate().map(|(position, concept)| (concept.concept_id, position)).collect::<BTreeMap<_, _>>();
        for (position, concept) in resolution.concepts.iter().enumerate() {
            for parent in &concept.parents {
                assert!(positions[&parent.concept_id] < position, "parent must precede child");
            }
        }
    }

    #[tokio::test]
    async fn concepts_and_children_are_expandable_on_demand() {
        let roots_response = app(service()).oneshot(Request::builder().uri("/v1/roots").body(Body::empty()).unwrap()).await.unwrap();
        let roots: TaxonomySlice = json(roots_response).await;
        let root = roots.concepts.iter().find(|concept| concept.preferred_label == "Arts & Media").expect("Arts & Media root");
        let uri = format!("/v1/concepts/{}/children", root.concept_id);
        let response = app(service()).oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let slice: TaxonomySlice = json(response).await;
        assert!(slice.concepts.iter().any(|concept| concept.preferred_label == "Music"));
        assert!(slice.requested_concept_ids.iter().all(|concept_id| slice.concepts.iter().any(|concept| &concept.concept_id == concept_id)));
    }

    #[tokio::test]
    async fn release_description_supports_bodyless_conditional_checks() {
        let service = service();
        assert_eq!(service.state.description.release_id, 1);
        let router = app(service);
        let response = router.clone().oneshot(Request::builder().method(Method::HEAD).uri("/v1/taxonomy").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let etag = response.headers()[header::ETAG].clone();
        assert!(response.into_body().collect().await.unwrap().to_bytes().is_empty());

        let response = router.oneshot(Request::builder().method(Method::HEAD).uri("/v1/taxonomy").header(header::IF_NONE_MATCH, etag).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(response.into_body().collect().await.unwrap().to_bytes().is_empty());
    }

    #[tokio::test]
    async fn immutable_snapshot_supports_conditional_downloads() {
        let router = app(service());
        let response = router.clone().oneshot(Request::builder().uri("/v1/snapshot").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], SNAPSHOT_MEDIA_TYPE);
        let etag = response.headers()[header::ETAG].clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.starts_with(b"SQLite format 3\0"));

        let response = router.oneshot(Request::builder().uri("/v1/snapshot").header(header::IF_NONE_MATCH, etag).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }
}
