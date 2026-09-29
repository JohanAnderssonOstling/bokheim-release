//! Bounded, loopback-only stress workload for the production HTTP and
//! PostgreSQL synchronization path.

use account_contract::{AuthResponse, EmailVerificationRequest, LoginRequest, PublicRegistrationRequest};
use reqwest::{Client, Url};
use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::io::Write;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sync_common::api::assets::{BlobManifestEntry, BlobManifestRequest, BlobManifestResponse};
use sync_common::api::libraries::LibraryNameRequest;
use sync_common::fixture_content_hash;
use sync_common::{
    ContentHash, DeclaredBlobReference, LibraryId, MutationId, PullStateResponse, ReplicaId, ReplicaSeq, StateCell, StateInventoryRequest, StateInventoryResponse, SyncExchangeRequest, SyncExchangeResponse, WireMutation, MAX_PUSH_MUTATIONS,
    MAX_STATE_INVENTORY_CELLS,
};
use tokio::sync::{Barrier, Semaphore};
use tokio::task::JoinSet;
use uuid::Uuid;

type AnyError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Copy)]
struct Workload {
    workers: usize,
    requests_per_worker: usize,
    events_per_request: usize,
}

#[derive(Clone)]
struct Identity {
    token: Arc<str>,
}

struct PullSample {
    elapsed: Duration,
    response_bytes: usize,
}

#[derive(Clone)]
struct BlobFixture {
    library_id: LibraryId,
    hash: Arc<str>,
    bytes: Arc<Vec<u8>>,
}

type CellKey = (String, String, String);
type ReplicaProjection = BTreeMap<CellKey, (ReplicaId, WireMutation)>;

struct ProjectedReplica {
    replica_id: ReplicaId,
    cursor: sync_common::SyncCursor,
    cells: ReplicaProjection,
}

fn bounded_env(name: &str, default: usize, maximum: usize) -> Result<usize, AnyError> {
    let value = match std::env::var(name) {
        Ok(raw) => raw.parse::<usize>().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{name} must be an integer")))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.into()),
    };
    if value == 0 || value > maximum {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name} must be between 1 and {maximum}")).into());
    }
    Ok(value)
}

fn parse_loopback_url(raw: &str) -> Result<Url, AnyError> {
    let mut url = Url::parse(raw)?;
    let loopback = match url.host_str() {
        Some("localhost") => true,
        Some(host) => host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().is_ok_and(|address| address.is_loopback()),
        None => false,
    };
    if url.scheme() != "http" || !loopback || url.username() != "" || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the stress harness only accepts an unauthenticated http:// loopback URL").into());
    }
    url.set_path("/");
    Ok(url)
}

fn loopback_url() -> Result<Url, AnyError> {
    let raw = std::env::var("BOKHEIM_STRESS_URL").unwrap_or_else(|_| "http://127.0.0.1:8080/".to_owned());
    parse_loopback_url(&raw)
}

fn endpoint(base: &Url, path: &str) -> Result<Url, AnyError> {
    Ok(base.join(path)?)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn wire_mutation(kind: &str, entity_key: impl Into<String>, entity_subkey: impl Into<String>, sequence: u64, changed_at: u64) -> Result<WireMutation, AnyError> {
    // A lifecycle mutation must declare whether it references a blob, because
    // that declaration is all the service knows about the book's bytes. This
    // workload measures relay throughput against synthetic keys and stores no
    // bytes, so it declares that it holds no reference.
    let blob_reference = (kind == sync_common::mutation_kind::BOOK_LIFECYCLE).then_some(DeclaredBlobReference { present: false, content_hash: None });
    Ok(WireMutation {
        origin: None,
        mutation_id: MutationId::new(),
        kind: kind.to_owned(),
        entity_key: entity_key.into(),
        entity_subkey: entity_subkey.into(),
        value: Vec::new(),
        conflict_rank: 0,
        blob_reference,
        changed_at,
        replica_seq: ReplicaSeq::new(sequence)?,
    })
}

fn test_epub(payload_bytes: usize, seed: usize) -> Result<Vec<u8>, AnyError> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let compressed = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    archive.start_file("mimetype", stored)?;
    archive.write_all(b"application/epub+zip")?;
    archive.start_file("META-INF/container.xml", compressed)?;
    archive.write_all(br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#)?;
    archive.start_file("OEBPS/content.opf", compressed)?;
    archive.write_all(br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#)?;
    archive.start_file("OEBPS/chapter.xhtml", stored)?;
    archive.write_all(br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><pre>"#)?;
    archive.write_all(format!("fixture-{seed}:").as_bytes())?;
    let mut payload = vec![0_u8; payload_bytes];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = b'a' + ((index.wrapping_mul(7).wrapping_add(seed.wrapping_mul(17))) % 26) as u8;
    }
    archive.write_all(&payload)?;
    archive.write_all(b"</pre></body></html>")?;
    Ok(archive.finish()?.into_inner())
}

fn percentile(sorted_micros: &[u128], percentile: usize) -> f64 {
    let index = ((sorted_micros.len() - 1) * percentile + 99) / 100;
    sorted_micros[index] as f64 / 1_000.0
}

fn report_latencies(label: &str, samples: &[Duration], wall: Duration) {
    let mut micros: Vec<_> = samples.iter().map(Duration::as_micros).collect();
    micros.sort_unstable();
    let throughput = samples.len() as f64 / wall.as_secs_f64();
    println!(
        "{label}: requests={} throughput={throughput:.1}/s p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms wall={:.2}s",
        samples.len(),
        percentile(&micros, 50),
        percentile(&micros, 95),
        percentile(&micros, 99),
        micros[micros.len() - 1] as f64 / 1_000.0,
        wall.as_secs_f64(),
    );
}

fn process_tree_cpu_ticks(root: u32) -> u64 {
    fn visit(pid: u32, seen: &mut std::collections::HashSet<u32>) -> u64 {
        if !seen.insert(pid) {
            return 0;
        }
        let ticks = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(')').map(|(_, fields)| fields.to_owned()))
            .map(|fields| {
                let fields: Vec<_> = fields.split_whitespace().collect();
                fields.get(11).and_then(|value| value.parse::<u64>().ok()).unwrap_or(0) + fields.get(12).and_then(|value| value.parse::<u64>().ok()).unwrap_or(0)
            })
            .unwrap_or(0);
        let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
        ticks + children.split_whitespace().filter_map(|child| child.parse::<u32>().ok()).map(|child| visit(child, seen)).sum::<u64>()
    }
    visit(root, &mut std::collections::HashSet::new())
}

fn profiled_process_ticks() -> (u64, u64) {
    let server = std::env::var("BOKHEIM_STRESS_SERVER_PID").ok().and_then(|value| value.parse::<u32>().ok()).map(process_tree_cpu_ticks).unwrap_or(0);
    let postgres = std::env::var("BOKHEIM_STRESS_POSTGRES_PID").ok().and_then(|value| value.parse::<u32>().ok()).map(process_tree_cpu_ticks).unwrap_or(0);
    (server, postgres)
}

fn process_io_bytes(pid: u32) -> (u64, u64) {
    let values = std::fs::read_to_string(format!("/proc/{pid}/io")).unwrap_or_default();
    let read = |name: &str| values.lines().find_map(|line| line.strip_prefix(name)).and_then(|value| value.trim().parse::<u64>().ok()).unwrap_or(0);
    (read("write_bytes:"), read("cancelled_write_bytes:"))
}

fn profiled_server_io() -> (u64, u64) {
    std::env::var("BOKHEIM_STRESS_SERVER_PID").ok().and_then(|value| value.parse::<u32>().ok()).map(process_io_bytes).unwrap_or((0, 0))
}

async fn login(client: &Client, base: &Url) -> Result<Identity, AnyError> {
    let email = std::env::var("BOKHEIM_STRESS_EMAIL").map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_STRESS_EMAIL is required when BOKHEIM_STRESS_TOKEN is not set"))?;
    let password = std::env::var("BOKHEIM_STRESS_PASSWORD").map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_STRESS_PASSWORD is required"))?;
    let encoded = sync_common::transport::encode(&LoginRequest { email, password })?;
    let response = client.post(endpoint(base, "api/auth/login")?).header(reqwest::header::CONTENT_TYPE, sync_common::transport::MEDIA_TYPE).header(reqwest::header::ACCEPT, sync_common::transport::MEDIA_TYPE).body(encoded).send().await?;
    let (auth, _): (AuthResponse, _) = decode_api_response(response).await?;
    Ok(Identity { token: auth.token.into() })
}

async fn register(client: &Client, base: &Url) -> Result<(), AnyError> {
    let email = std::env::var("BOKHEIM_STRESS_EMAIL").map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_STRESS_EMAIL is required"))?;
    let password = std::env::var("BOKHEIM_STRESS_PASSWORD").map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_STRESS_PASSWORD is required"))?;
    let encoded = sync_common::transport::encode(&PublicRegistrationRequest { email, password })?;
    let response = client.post(endpoint(base, "api/auth/register")?).header(reqwest::header::CONTENT_TYPE, sync_common::transport::MEDIA_TYPE).header(reqwest::header::ACCEPT, sync_common::transport::MEDIA_TYPE).body(encoded).send().await?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!("registration failed with {}", response.status())).into());
    }
    Ok(())
}

async fn verify_email(client: &Client, base: &Url) -> Result<Identity, AnyError> {
    let email = std::env::var("BOKHEIM_STRESS_EMAIL").map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_STRESS_EMAIL is required"))?;
    let pin = std::env::var("BOKHEIM_STRESS_VERIFICATION_PIN").map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_STRESS_VERIFICATION_PIN is required"))?;
    let encoded = sync_common::transport::encode(&EmailVerificationRequest { email, pin })?;
    let response =
        client.post(endpoint(base, "api/auth/verify-email")?).header(reqwest::header::CONTENT_TYPE, sync_common::transport::MEDIA_TYPE).header(reqwest::header::ACCEPT, sync_common::transport::MEDIA_TYPE).body(encoded).send().await?;
    let (auth, _): (AuthResponse, _) = decode_api_response(response).await?;
    Ok(Identity { token: auth.token.into() })
}

async fn create_library(client: &Client, base: &Url, identity: &Identity, library_id: &LibraryId) -> Result<(), AnyError> {
    let request = LibraryNameRequest { library_id: *library_id, library_name: format!("Stress {library_id}") };
    let _: (sync_common::api::libraries::LibrarySummary, _, _) = post_api(client, endpoint(base, "api/libraries")?, identity, &request).await?;
    Ok(())
}

async fn decode_api_response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<(T, usize), AnyError> {
    let status = response.status();
    if !status.is_success() {
        return Err(io::Error::other(format!("API request failed with {status}: {}", response.text().await.unwrap_or_default())).into());
    }
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or_default().to_owned();
    if response.headers().get(reqwest::header::CONTENT_ENCODING).and_then(|value| value.to_str().ok()).map(str::trim).is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("identity")) {
        return Err(io::Error::other("compressed stress responses are unsupported").into());
    }
    let bytes = response.bytes().await?;
    let wire_bytes = bytes.len();
    if !sync_common::transport::is_current_media_type(&content_type) {
        return Err(io::Error::other(format!("unsupported stress response content type {content_type}")).into());
    }
    let decoded = sync_common::transport::decode(&bytes, sync_common::transport::MAX_DECODED_RESPONSE_BYTES)?;
    Ok((decoded, wire_bytes))
}

async fn post_api<T: serde::Serialize, R: serde::de::DeserializeOwned>(client: &Client, url: Url, identity: &Identity, value: &T) -> Result<(R, usize, usize), AnyError> {
    let encoded = sync_common::transport::encode(value)?;
    let request_bytes = encoded.len();
    let request = client.post(url).bearer_auth(identity.token.as_ref()).header(reqwest::header::CONTENT_TYPE, sync_common::transport::MEDIA_TYPE).header(reqwest::header::ACCEPT, sync_common::transport::MEDIA_TYPE);
    let (response, response_bytes) = decode_api_response(request.body(encoded).send().await?).await?;
    Ok((response, request_bytes, response_bytes))
}

/// The server intentionally exposes one atomic exchange endpoint. A pull is
/// therefore an exchange with no local mutations.
async fn pull_page(client: &Client, base: &Url, identity: &Identity, library_id: LibraryId, replica_id: ReplicaId, cursor: sync_common::SyncCursor) -> Result<(PullStateResponse, usize), AnyError> {
    let request = SyncExchangeRequest { library_id, replica_id, mutations: Vec::new(), cursor };
    let (response, _, response_bytes): (SyncExchangeResponse, _, _) = post_api(client, endpoint(base, "api/sync/exchange")?, identity, &request).await?;
    Ok((response.pull, response_bytes))
}

fn apply_projection(cells: &mut ReplicaProjection, pull: &PullStateResponse) {
    for change in &pull.mutations {
        let mutation = &change.mutation;
        cells.insert((mutation.kind.clone(), mutation.entity_key.clone(), mutation.entity_subkey.clone()), (change.replica_id, mutation.clone()));
    }
}

async fn catch_up_projection(client: &Client, base: &Url, identity: &Identity, library_id: LibraryId, replica: &mut ProjectedReplica) -> Result<(), AnyError> {
    loop {
        let (pull, _) = pull_page(client, base, identity, library_id, replica.replica_id, replica.cursor).await?;
        apply_projection(&mut replica.cells, &pull);
        replica.cursor = pull.next_cursor;
        if !pull.has_more {
            return Ok(());
        }
    }
}

/// Keeps a materialized projection for every logical replica while atomic
/// reading writes race generic mixed batches, then requires all projections to
/// equal a fresh server projection after their final pull.
async fn run_replica_convergence_guard(client: &Client, base: &Url, identity: &Identity, workload: Workload) -> Result<(), AnyError> {
    let library_id = Uuid::new_v4();
    create_library(client, base, identity, &library_id).await?;
    let replica_count = workload.workers.min(16);
    let rounds = workload.requests_per_worker.min(20);
    let barrier = Arc::new(Barrier::new(replica_count));
    let mut tasks = JoinSet::new();

    for worker in 0..replica_count {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let replica_id = Uuid::new_v4();
            let mut projected = ProjectedReplica { replica_id, cursor: sync_common::SyncCursor::default(), cells: BTreeMap::new() };
            let clock = now_ms().saturating_add(worker as u64);
            barrier.wait().await;
            for round in 0..rounds {
                let first_sequence = (round * 2 + 1) as u64;
                let mut reading = wire_mutation("reading_position", "shared-reading", "", first_sequence, clock)?;
                reading.value = format!("replica-{worker}-round-{round}").into_bytes();
                let mutations = if round % 2 == 0 {
                    vec![reading]
                } else {
                    let mut metadata = wire_mutation("directory_name", format!("replica-{worker}"), "", first_sequence + 1, clock)?;
                    metadata.value = format!("round-{round}").into_bytes();
                    vec![reading, metadata]
                };
                let request = SyncExchangeRequest { library_id, replica_id, mutations, cursor: projected.cursor };
                let (response, _, _): (SyncExchangeResponse, _, _) = post_api(&client, endpoint(&base, "api/sync/exchange")?, &identity, &request).await?;
                if !response.push.rejected.is_empty() || response.push.accepted.len() != request.mutations.len() {
                    return Err::<ProjectedReplica, AnyError>(io::Error::other("convergence exchange did not acknowledge every mutation").into());
                }
                apply_projection(&mut projected.cells, &response.pull);
                projected.cursor = response.pull.next_cursor;
                if response.pull.has_more {
                    catch_up_projection(&client, &base, &identity, library_id, &mut projected).await?;
                }
            }
            Ok(projected)
        });
    }

    let mut replicas = Vec::with_capacity(replica_count);
    while let Some(result) = tasks.join_next().await {
        replicas.push(result.map_err(|error| io::Error::other(format!("convergence worker failed: {error}")))??);
    }
    for replica in &mut replicas {
        catch_up_projection(client, base, identity, library_id, replica).await?;
    }
    let mut server = ProjectedReplica { replica_id: Uuid::new_v4(), cursor: sync_common::SyncCursor::default(), cells: BTreeMap::new() };
    catch_up_projection(client, base, identity, library_id, &mut server).await?;
    for replica in &replicas {
        if replica.cells != server.cells {
            return Err(io::Error::other(format!("replica {} diverged: replica_cells={} server_cells={}", replica.replica_id, replica.cells.len(), server.cells.len())).into());
        }
    }
    println!("replica convergence: replicas={replica_count} rounds={rounds} cells={} status=equal", server.cells.len());
    Ok(())
}

async fn run_library_ingest(client: &Client, base: &Url, identity: &Identity, book_count: usize, directory_count: usize) -> Result<(), AnyError> {
    let wire_label = "protobuf";
    let library_id = Uuid::new_v4();
    create_library(client, base, identity, &library_id).await?;
    let replica_id = Uuid::new_v4();
    let clock = now_ms();
    let mut mutations = Vec::with_capacity(directory_count * 3 + book_count * 2);
    let mut sequence = 1_u64;
    for index in 1..=directory_count {
        let dir_id = format!("ingest-dir-{index}");
        mutations.push(wire_mutation("directory_name", dir_id.as_str(), "", sequence, clock)?);
        sequence += 1;
        mutations.push(wire_mutation("directory_parent", dir_id.as_str(), "", sequence, clock)?);
        sequence += 1;
        mutations.push(wire_mutation("directory_lifecycle", dir_id.as_str(), "", sequence, clock)?);
        sequence += 1;
    }
    for index in 1..=book_count {
        let content_hash = index.to_string();
        let dir_id = format!("ingest-dir-{}", (index - 1) % directory_count + 1);
        mutations.push(wire_mutation("book_lifecycle", content_hash.clone(), "", sequence, clock)?);
        sequence += 1;
        mutations.push(wire_mutation("placement", content_hash, dir_id.as_str(), sequence, clock)?);
        sequence += 1;
    }

    let mutation_count = mutations.len();
    let push_started = Instant::now();
    let mut push_samples = Vec::new();
    let mut request_bytes = 0_usize;
    for page in mutations.chunks(MAX_PUSH_MUTATIONS) {
        let request = SyncExchangeRequest { library_id: library_id.clone(), replica_id: replica_id.clone(), mutations: page.to_vec(), cursor: sync_common::SyncCursor::default() };
        let request_started = Instant::now();
        let (response, sent_bytes, _response_bytes): (SyncExchangeResponse, _, _) = post_api(client, endpoint(base, "api/sync/exchange")?, identity, &request).await?;
        request_bytes += sent_bytes;
        if !response.push.rejected.is_empty() || response.push.accepted.len() != page.len() {
            return Err(io::Error::other(format!("library ingest acknowledged {} mutations and rejected {} of {}", response.push.accepted.len(), response.push.rejected.len(), page.len())).into());
        }
        push_samples.push(request_started.elapsed());
    }
    let push_wall = push_started.elapsed();
    report_latencies("large-library ingest pages", &push_samples, push_wall);
    println!(
        "large-library ingest: format={wire_label} books={book_count} directories={directory_count} mutations={mutation_count} pages={} wire_request_bytes={:.2}MiB book_throughput={:.0}/s mutation_throughput={:.0}/s wall={:.3}s",
        push_samples.len(),
        request_bytes as f64 / (1024.0 * 1024.0),
        book_count as f64 / push_wall.as_secs_f64(),
        mutation_count as f64 / push_wall.as_secs_f64(),
        push_wall.as_secs_f64(),
    );

    let pull_started = Instant::now();
    let mut cursor = sync_common::SyncCursor::default();
    let mut pulled = 0_usize;
    let mut pull_pages = 0_usize;
    let mut response_bytes = 0_usize;
    loop {
        let request = SyncExchangeRequest { library_id, replica_id: Uuid::new_v4(), mutations: Vec::new(), cursor };
        let (response, _, wire_bytes): (SyncExchangeResponse, _, _) = post_api(client, endpoint(base, "api/sync/exchange")?, identity, &request).await?;
        let page = response.pull;
        response_bytes += wire_bytes;
        pull_pages += 1;
        pulled += page.mutations.len();
        cursor = page.next_cursor;
        if !page.has_more {
            break;
        }
    }
    let pull_wall = pull_started.elapsed();
    if pulled != mutation_count {
        return Err(io::Error::other(format!("library ingest pulled {pulled} canonical states after writing {mutation_count}")).into());
    }
    println!(
        "large-library full pull: format={wire_label} states={pulled} pages={pull_pages} wire_response_bytes={:.2}MiB state_throughput={:.0}/s wall={:.3}s",
        response_bytes as f64 / (1024.0 * 1024.0),
        pulled as f64 / pull_wall.as_secs_f64(),
        pull_wall.as_secs_f64(),
    );
    Ok(())
}

fn inventory_cells(count: usize, key_offset: usize) -> Vec<StateCell> {
    (0..count).map(|index| StateCell { kind: "metadata".to_owned(), entity_key: format!("inventory-cell-{}", key_offset + index), entity_subkey: String::new() }).collect()
}

async fn run_inventory_case(client: &Client, base: &Url, identity: &Identity, library_id: LibraryId, label: &str, cells: &[StateCell], expected_missing: &[StateCell], repetitions: usize) -> Result<(), AnyError> {
    let expected: std::collections::HashSet<_> = expected_missing.iter().cloned().collect();
    let phase_started = Instant::now();
    let mut cycle_samples = Vec::with_capacity(repetitions);
    let mut request_samples = Vec::new();
    let mut request_bytes = 0_usize;
    let mut response_bytes = 0_usize;
    for _ in 0..repetitions {
        let cycle_started = Instant::now();
        let mut observed = std::collections::HashSet::new();
        for page in cells.chunks(MAX_STATE_INVENTORY_CELLS) {
            let request = StateInventoryRequest { library_id, cells: page.to_vec() };
            let request_started = Instant::now();
            let (response, sent, received): (StateInventoryResponse, _, _) = post_api(client, endpoint(base, "api/sync/inventory")?, identity, &request).await?;
            request_samples.push(request_started.elapsed());
            request_bytes += sent;
            response_bytes += received;
            for cell in response.missing {
                if !page.contains(&cell) || !observed.insert(cell) {
                    return Err(io::Error::other(format!("{label} inventory returned an unrequested or duplicate cell")).into());
                }
            }
        }
        if observed != expected {
            return Err(io::Error::other(format!("{label} inventory returned {} missing cells; expected {}", observed.len(), expected.len())).into());
        }
        cycle_samples.push(cycle_started.elapsed());
    }
    let wall = phase_started.elapsed();
    report_latencies(&format!("inventory {label} cycle"), &cycle_samples, wall);
    report_latencies(&format!("inventory {label} request"), &request_samples, wall);
    println!(
        "inventory {label}: cells={} missing={} repetitions={repetitions} pages/cycle={} request={:.2}MiB response={:.2}MiB",
        cells.len(),
        expected.len(),
        cells.len().div_ceil(MAX_STATE_INVENTORY_CELLS),
        request_bytes as f64 / (1024.0 * 1024.0),
        response_bytes as f64 / (1024.0 * 1024.0),
    );
    Ok(())
}

async fn run_inventory_profile(client: &Client, base: &Url, identity: &Identity, cell_count: usize, repetitions: usize) -> Result<(), AnyError> {
    let library_id = Uuid::new_v4();
    create_library(client, base, identity, &library_id).await?;
    let replica_id = Uuid::new_v4();
    let cells = inventory_cells(cell_count, 0);
    let clock = now_ms();
    let seed_started = Instant::now();
    for (page_index, page) in cells.chunks(MAX_PUSH_MUTATIONS).enumerate() {
        let mutations =
            page.iter().enumerate().map(|(index, cell)| wire_mutation(&cell.kind, cell.entity_key.clone(), cell.entity_subkey.clone(), (page_index * MAX_PUSH_MUTATIONS + index + 1) as u64, clock)).collect::<Result<Vec<_>, _>>()?;
        let request = SyncExchangeRequest { library_id, replica_id, mutations, cursor: sync_common::SyncCursor::default() };
        let (response, _, _): (SyncExchangeResponse, _, _) = post_api(client, endpoint(base, "api/sync/exchange")?, identity, &request).await?;
        if !response.push.rejected.is_empty() || response.push.accepted.len() != page.len() {
            return Err(io::Error::other("inventory profile could not seed every server cell").into());
        }
    }
    println!("inventory seed: cells={cell_count} wall={:.3}s", seed_started.elapsed().as_secs_f64());

    run_inventory_case(client, base, identity, library_id, "all-present", &cells, &[], repetitions).await?;

    let mut one_missing = cells.clone();
    one_missing[cell_count - 1] = inventory_cells(1, cell_count)[0].clone();
    run_inventory_case(client, base, identity, library_id, "one-missing", &one_missing, &one_missing[cell_count - 1..], repetitions).await?;

    let missing_count = cell_count.div_ceil(100);
    let mut percent_missing = cells;
    let replacements = inventory_cells(missing_count, cell_count + 1);
    percent_missing[cell_count - missing_count..].clone_from_slice(&replacements);
    run_inventory_case(client, base, identity, library_id, "one-percent-missing", &percent_missing, &replacements, repetitions).await?;
    Ok(())
}

fn changes(worker: usize, request_index: usize, workload: Workload, id_offset: u64, clock: u64) -> Vec<WireMutation> {
    (0..workload.events_per_request)
        .map(|event_index| {
            let local = request_index * workload.events_per_request + event_index + 1;
            let globally_unique = worker * workload.requests_per_worker * workload.events_per_request + local;
            wire_mutation("book_lifecycle", (id_offset + globally_unique as u64).to_string(), "", local as u64, clock).expect("bounded workload creates valid mutations")
        })
        .collect()
}

async fn run_pushes(label: &str, client: &Client, base: &Url, identity: &Identity, libraries: &[LibraryId], workload: Workload, id_offset: u64) -> Result<(), AnyError> {
    let barrier = Arc::new(Barrier::new(workload.workers));
    let started = Instant::now();
    let mut tasks: JoinSet<Result<Vec<Duration>, AnyError>> = JoinSet::new();
    for worker in 0..workload.workers {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let library_id = libraries[worker % libraries.len()].clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let replica_id = Uuid::new_v4();
            let clock = now_ms();
            let mut samples = Vec::with_capacity(workload.requests_per_worker);
            barrier.wait().await;
            for request_index in 0..workload.requests_per_worker {
                let request = SyncExchangeRequest { library_id: library_id.clone(), replica_id: replica_id.clone(), mutations: changes(worker, request_index, workload, id_offset, clock), cursor: sync_common::SyncCursor::default() };
                let request_started = Instant::now();
                let (response, _, _): (SyncExchangeResponse, _, _) = post_api(&client, endpoint(&base, "api/sync/exchange")?, &identity, &request).await?;
                if !response.push.rejected.is_empty() || response.push.accepted.len() != workload.events_per_request {
                    return Err(io::Error::other(format!("push acknowledged {} mutations and rejected {} of {}", response.push.accepted.len(), response.push.rejected.len(), workload.events_per_request)).into());
                }
                samples.push(request_started.elapsed());
            }
            Ok(samples)
        });
    }

    let mut samples = Vec::with_capacity(workload.workers * workload.requests_per_worker);
    while let Some(result) = tasks.join_next().await {
        samples.extend(result.map_err(|error| io::Error::other(format!("stress worker failed: {error}")))??);
    }
    report_latencies(label, &samples, started.elapsed());
    Ok(())
}

/// Models the hot reading path after client-side coalescing: each device sends
/// one latest reading-position register value per synchronization request.
async fn run_position_pushes(label: &str, client: &Client, base: &Url, identity: &Identity, libraries: &[LibraryId], workload: Workload, content_hash_offset: u64) -> Result<(), AnyError> {
    let barrier = Arc::new(Barrier::new(workload.workers));
    let started = Instant::now();
    let mut tasks: JoinSet<Result<Vec<Duration>, AnyError>> = JoinSet::new();
    for worker in 0..workload.workers {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let library_id = libraries[worker % libraries.len()].clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let replica_id = Uuid::new_v4();
            let clock = now_ms();
            let content_hash = fixture_content_hash(content_hash_offset + worker as u64 + 1);
            let mut samples = Vec::with_capacity(workload.requests_per_worker);
            barrier.wait().await;
            for request_index in 0..workload.requests_per_worker {
                let sequence = request_index + 1;
                let request = SyncExchangeRequest {
                    library_id: library_id.clone(),
                    replica_id: replica_id.clone(),
                    mutations: vec![wire_mutation("reading_position", content_hash.to_string(), "", sequence as u64, clock)?],
                    cursor: sync_common::SyncCursor::default(),
                };
                let request_started = Instant::now();
                let (response, _, _): (SyncExchangeResponse, _, _) = post_api(&client, endpoint(&base, "api/sync/exchange")?, &identity, &request).await?;
                if !response.push.rejected.is_empty() || response.push.accepted.len() != 1 {
                    return Err(io::Error::other(format!("position push acknowledged {} mutations and rejected {}", response.push.accepted.len(), response.push.rejected.len())).into());
                }
                samples.push(request_started.elapsed());
            }
            Ok(samples)
        });
    }

    let mut samples = Vec::with_capacity(workload.workers * workload.requests_per_worker);
    while let Some(result) = tasks.join_next().await {
        samples.extend(result.map_err(|error| io::Error::other(format!("position stress worker failed: {error}")))??);
    }
    report_latencies(label, &samples, started.elapsed());
    Ok(())
}

/// Measures the user-visible hot path: one exchange publishes a coalesced
/// position and returns the incremental page from the committed snapshot.
async fn run_position_sync_cycles(client: &Client, base: &Url, identity: &Identity, libraries: &[LibraryId], workload: Workload, content_hash_offset: u64) -> Result<(), AnyError> {
    let barrier = Arc::new(Barrier::new(workload.workers));
    let started = Instant::now();
    let mut tasks: JoinSet<Result<Vec<Duration>, AnyError>> = JoinSet::new();
    for worker in 0..workload.workers {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let library_id = libraries[worker % libraries.len()].clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let replica_id = Uuid::new_v4();
            let content_hash = fixture_content_hash(content_hash_offset + worker as u64 + 1);
            let mut cursor = sync_common::SyncCursor::default();
            loop {
                let (pulled, _) = pull_page(&client, &base, &identity, library_id.clone(), replica_id.clone(), cursor).await?;
                cursor = pulled.next_cursor;
                if !pulled.has_more {
                    break;
                }
            }

            let clock = now_ms().saturating_add(1_000);
            let mut samples = Vec::with_capacity(workload.requests_per_worker);
            barrier.wait().await;
            for request_index in 0..workload.requests_per_worker {
                let sequence = request_index + 1;
                let request = SyncExchangeRequest { library_id: library_id.clone(), replica_id: replica_id.clone(), mutations: vec![wire_mutation("reading_position", content_hash.to_string(), "", sequence as u64, clock)?], cursor };
                let cycle_started = Instant::now();
                let (exchanged, _, _): (SyncExchangeResponse, _, _) = post_api(&client, endpoint(&base, "api/sync/exchange")?, &identity, &request).await?;
                if !exchanged.push.rejected.is_empty() || exchanged.push.accepted.len() != 1 {
                    return Err(io::Error::other("position exchange was not acknowledged exactly once").into());
                }
                let pulled = exchanged.pull;
                if pulled.has_more || pulled.mutations.len() != 1 {
                    return Err(io::Error::other(format!("position exchange observed {} mutations with has_more={}", pulled.mutations.len(), pulled.has_more)).into());
                }
                cursor = pulled.next_cursor;
                samples.push(cycle_started.elapsed());
            }
            Ok(samples)
        });
    }

    let mut samples = Vec::with_capacity(workload.workers * workload.requests_per_worker);
    while let Some(result) = tasks.join_next().await {
        samples.extend(result.map_err(|error| io::Error::other(format!("position cycle worker failed: {error}")))??);
    }
    report_latencies("sharded reading-position sync cycle", &samples, started.elapsed());
    Ok(())
}

/// Measures the second-device hot path: a writer publishes one position and an
/// already-caught-up reader retrieves that remote position from the same
/// library cursor. Unlike the atomic exchange benchmark, this exercises the
/// pull router rather than returning the writer's own mutation directly.
async fn run_remote_position_pull_cycles(client: &Client, base: &Url, identity: &Identity, libraries: &[LibraryId], workload: Workload, content_hash_offset: u64) -> Result<(), AnyError> {
    let barrier = Arc::new(Barrier::new(workload.workers));
    let started = Instant::now();
    let mut tasks: JoinSet<Result<Vec<Duration>, AnyError>> = JoinSet::new();
    for worker in 0..workload.workers {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let library_id = libraries[worker % libraries.len()].clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let writer_id = Uuid::new_v4();
            let reader_id = Uuid::new_v4();
            let content_hash = fixture_content_hash(content_hash_offset + worker as u64 + 1);
            let mut cursor = sync_common::SyncCursor::default();
            loop {
                let (pulled, _) = pull_page(&client, &base, &identity, library_id.clone(), reader_id.clone(), cursor).await?;
                cursor = pulled.next_cursor;
                if !pulled.has_more {
                    break;
                }
            }

            let clock = now_ms().saturating_add(2_000);
            let mut samples = Vec::with_capacity(workload.requests_per_worker);
            barrier.wait().await;
            for request_index in 0..workload.requests_per_worker {
                let sequence = request_index + 1;
                let push = SyncExchangeRequest {
                    library_id: library_id.clone(),
                    replica_id: writer_id.clone(),
                    mutations: vec![wire_mutation("reading_position", content_hash.to_string(), "", sequence as u64, clock)?],
                    cursor: sync_common::SyncCursor::default(),
                };
                let (acknowledged, _, _): (SyncExchangeResponse, _, _) = post_api(&client, endpoint(&base, "api/sync/exchange")?, &identity, &push).await?;
                if acknowledged.push.accepted.len() != 1 || !acknowledged.push.rejected.is_empty() {
                    return Err(io::Error::other("remote position writer was not acknowledged exactly once").into());
                }

                let pull_started = Instant::now();
                let (pulled, _) = pull_page(&client, &base, &identity, library_id.clone(), reader_id.clone(), cursor).await?;
                if pulled.has_more || pulled.mutations.len() != 1 || pulled.mutations[0].mutation.kind != "reading_position" {
                    return Err(io::Error::other(format!("remote position pull observed {} mutations with has_more={}", pulled.mutations.len(), pulled.has_more)).into());
                }
                cursor = pulled.next_cursor;
                samples.push(pull_started.elapsed());
            }
            Ok(samples)
        });
    }

    let mut samples = Vec::with_capacity(workload.workers * workload.requests_per_worker);
    while let Some(result) = tasks.join_next().await {
        samples.extend(result.map_err(|error| io::Error::other(format!("remote position pull worker failed: {error}")))??);
    }
    report_latencies("sharded remote reading-position pull cycle", &samples, started.elapsed());
    Ok(())
}

async fn pull_once(client: &Client, base: &Url, identity: &Identity, library_id: LibraryId, replica_id: ReplicaId, expected_events: usize) -> Result<PullSample, AnyError> {
    let started = Instant::now();
    let mut cursor = sync_common::SyncCursor::default();
    let mut response_bytes = 0_usize;
    let mut pulled_events = 0_usize;
    loop {
        let (pulled, wire_bytes) = pull_page(client, base, identity, library_id.clone(), replica_id.clone(), cursor).await?;
        response_bytes += wire_bytes;
        if pulled.has_more && pulled.mutations.is_empty() {
            return Err(io::Error::other("pull returned an empty non-final page").into());
        }
        pulled_events += pulled.mutations.len();
        cursor = pulled.next_cursor;
        if !pulled.has_more {
            break;
        }
    }
    if pulled_events != expected_events {
        return Err(io::Error::other(format!("pull returned {pulled_events} events; expected {expected_events}")).into());
    }
    Ok(PullSample { elapsed: started.elapsed(), response_bytes })
}

async fn verify_sharded_pulls(client: &Client, base: &Url, identity: &Identity, libraries: &[LibraryId], expected_events: usize) -> Result<(), AnyError> {
    let started = Instant::now();
    let mut samples = Vec::with_capacity(libraries.len());
    let mut total_bytes = 0_usize;
    for library in libraries {
        let sample = pull_once(client, base, identity, *library, Uuid::new_v4(), expected_events).await?;
        samples.push(sample.elapsed);
        total_bytes += sample.response_bytes;
    }
    report_latencies("sharded full pull", &samples, started.elapsed());
    println!("sharded full pull: aggregate_response={:.2}MiB", total_bytes as f64 / (1024.0 * 1024.0));
    Ok(())
}

async fn run_pull_fanout(client: &Client, base: &Url, identity: &Identity, library: &LibraryId, fanout: usize, expected_events: usize) -> Result<(), AnyError> {
    let barrier = Arc::new(Barrier::new(fanout));
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for _ in 0..fanout {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let library = library.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            pull_once(&client, &base, &identity, library, Uuid::new_v4(), expected_events).await
        });
    }
    let mut samples = Vec::with_capacity(fanout);
    let mut total_bytes = 0_usize;
    while let Some(result) = tasks.join_next().await {
        let sample = result.map_err(|error| io::Error::other(format!("pull worker failed: {error}")))??;
        samples.push(sample.elapsed);
        total_bytes += sample.response_bytes;
    }
    report_latencies("contended full-pull fanout", &samples, started.elapsed());
    println!("contended full-pull fanout: aggregate_response={:.2}MiB", total_bytes as f64 / (1024.0 * 1024.0));
    Ok(())
}

async fn run_blob_transfers(client: &Client, base: &Url, identity: &Identity, libraries: &[LibraryId], payload_bytes: usize) -> Result<(), AnyError> {
    let fixtures: Vec<_> = libraries
        .iter()
        .enumerate()
        .map(|(index, library_id)| {
            let bytes = test_epub(payload_bytes, index)?;
            let hash: Arc<str> = blake3::hash(&bytes).to_hex().to_string().into();
            Ok::<_, AnyError>(BlobFixture { library_id: library_id.clone(), hash, bytes: Arc::new(bytes) })
        })
        .collect::<Result<_, _>>()?;
    let total_bytes: usize = fixtures.iter().map(|fixture| fixture.bytes.len()).sum();

    let upload_started = Instant::now();
    let barrier = Arc::new(Barrier::new(fixtures.len()));
    // The production default deliberately permits four staged uploads per
    // account. Keep the benchmark at that boundary instead of treating the
    // expected 429 backpressure from a 16-way burst as a transport failure.
    let upload_concurrency = bounded_env("BOKHEIM_STRESS_UPLOAD_CONCURRENCY", 4, 256)?.min(fixtures.len());
    let upload_permits = Arc::new(Semaphore::new(upload_concurrency));
    let mut tasks = JoinSet::new();
    for fixture in fixtures.iter().cloned() {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let barrier = barrier.clone();
        let upload_permits = upload_permits.clone();
        tasks.spawn(async move {
            let url = endpoint(&base, &format!("api/libraries/{}/blobs/{}", fixture.library_id, fixture.hash))?;
            barrier.wait().await;
            let started = Instant::now();
            let _permit = upload_permits.acquire_owned().await.map_err(|_| io::Error::other("upload concurrency gate closed"))?;
            let response = client.put(url).bearer_auth(identity.token.as_ref()).body(fixture.bytes.as_ref().clone()).send().await?;
            let status = response.status();
            if !status.is_success() {
                return Err::<Duration, AnyError>(io::Error::other(format!("blob upload failed with {status}: {}", response.text().await.unwrap_or_default())).into());
            }
            Ok(started.elapsed())
        });
    }
    let mut upload_samples = Vec::with_capacity(fixtures.len());
    while let Some(result) = tasks.join_next().await {
        upload_samples.push(result.map_err(|error| io::Error::other(format!("upload worker failed: {error}")))??);
    }
    let upload_wall = upload_started.elapsed();
    report_latencies("validated blob upload", &upload_samples, upload_wall);
    println!("validated blob upload: transferred={:.2}MiB throughput={:.1}MiB/s", total_bytes as f64 / (1024.0 * 1024.0), total_bytes as f64 / (1024.0 * 1024.0) / upload_wall.as_secs_f64());

    let download_started = Instant::now();
    let barrier = Arc::new(Barrier::new(fixtures.len()));
    let mut tasks = JoinSet::new();
    for fixture in fixtures {
        let client = client.clone();
        let base = base.clone();
        let identity = identity.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let url = endpoint(&base, &format!("api/libraries/{}/blobs/{}", fixture.library_id, fixture.hash))?;
            barrier.wait().await;
            let started = Instant::now();
            let response = client.get(url).bearer_auth(identity.token.as_ref()).send().await?;
            let status = response.status();
            if !status.is_success() {
                return Err::<Duration, AnyError>(io::Error::other(format!("blob download failed with {status}: {}", response.text().await.unwrap_or_default())).into());
            }
            let downloaded = response.bytes().await?;
            if downloaded.as_ref() != fixture.bytes.as_slice() {
                return Err(io::Error::other("downloaded blob differs from the validated upload").into());
            }
            Ok(started.elapsed())
        });
    }
    let mut download_samples = Vec::with_capacity(libraries.len());
    while let Some(result) = tasks.join_next().await {
        download_samples.push(result.map_err(|error| io::Error::other(format!("download worker failed: {error}")))??);
    }
    let download_wall = download_started.elapsed();
    report_latencies("verified blob download", &download_samples, download_wall);
    println!("verified blob download: transferred={:.2}MiB throughput={:.1}MiB/s", total_bytes as f64 / (1024.0 * 1024.0), total_bytes as f64 / (1024.0 * 1024.0) / download_wall.as_secs_f64());
    Ok(())
}

async fn run_blob_manifest_profile(client: &Client, base: &Url, identity: &Identity, book_count: usize, repetitions: usize) -> Result<(), AnyError> {
    let library_id = Uuid::new_v4();
    create_library(client, base, identity, &library_id).await?;
    let fixtures: Vec<_> = (0..book_count)
        .map(|index| {
            let bytes = test_epub(1, index)?;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            Ok::<_, AnyError>((ContentHash::new(&hash), Arc::new(bytes)))
        })
        .collect::<Result<_, _>>()?;

    let seed_started = Instant::now();
    for group in fixtures.chunks(32) {
        let mut tasks = JoinSet::new();
        for (hash, bytes) in group {
            let client = client.clone();
            let base = base.clone();
            let identity = identity.clone();
            let library_id = library_id.clone();
            let hash = hash.clone();
            let bytes = bytes.clone();
            tasks.spawn(async move {
                let response = client.put(endpoint(&base, &format!("api/libraries/{library_id}/blobs/{}", hash.as_str()))?).bearer_auth(identity.token.as_ref()).body(bytes.as_ref().clone()).send().await?;
                let status = response.status();
                if !status.is_success() {
                    return Err::<(), AnyError>(io::Error::other(format!("asset profile seed failed with {status}: {}", response.text().await.unwrap_or_default())).into());
                }
                Ok(())
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.map_err(|error| io::Error::other(format!("asset seed worker failed: {error}")))??;
        }
    }
    println!("asset profile seed: blobs={book_count} wall={:.3}s", seed_started.elapsed().as_secs_f64());

    let manifest: Vec<_> = fixtures.iter().map(|(hash, bytes)| BlobManifestEntry { content_hash: hash.clone(), checksum: hash.clone(), size_bytes: bytes.len() as u64 }).collect();
    let mut batch_samples = Vec::with_capacity(repetitions);
    let batch_cpu_before = profiled_process_ticks();
    let batch_wall = Instant::now();
    for _ in 0..repetitions {
        let started = Instant::now();
        let (response, _, _): (BlobManifestResponse, _, _) = post_api(client, endpoint(&base, &format!("api/libraries/{library_id}/blobs/negotiate"))?, identity, &BlobManifestRequest { blobs: manifest.clone() }).await?;
        if response.owned.len() != book_count || !response.claimed.is_empty() || !response.upload.is_empty() || !response.rejected.is_empty() {
            return Err(io::Error::other(format!("batch negotiation did not classify all {book_count} seeded blobs as owned")).into());
        }
        batch_samples.push(started.elapsed());
    }
    let batch_elapsed = batch_wall.elapsed();
    let batch_cpu_after = profiled_process_ticks();
    report_latencies("batched manifest negotiation", &batch_samples, batch_elapsed);
    println!("batch discovery CPU: server_ticks={} postgres_ticks={}", batch_cpu_after.0.saturating_sub(batch_cpu_before.0), batch_cpu_after.1.saturating_sub(batch_cpu_before.1));
    Ok(())
}

#[derive(Clone, Copy)]
enum UploadProfileKind {
    Valid,
    Unsupported,
    StructurallyInvalid,
}

fn streaming_profile_body(bytes: Arc<Vec<u8>>, consumed: Arc<AtomicUsize>) -> reqwest::Body {
    let stream = futures_util::stream::unfold((bytes, consumed, 0_usize), |(bytes, consumed, offset)| async move {
        if offset == bytes.len() {
            return None;
        }
        let end = (offset + 64 * 1024).min(bytes.len());
        consumed.fetch_add(end - offset, Ordering::SeqCst);
        Some((Ok::<_, io::Error>(bytes[offset..end].to_vec()), (bytes, consumed, end)))
    });
    reqwest::Body::wrap_stream(stream)
}

async fn profile_upload_kind(client: &Client, base: &Url, identity: &Identity, library_id: &LibraryId, kind: UploadProfileKind, payload_bytes: usize, repetitions: usize) -> Result<(), AnyError> {
    let label = match kind {
        UploadProfileKind::Valid => "valid",
        UploadProfileKind::Unsupported => "unsupported-signature",
        UploadProfileKind::StructurallyInvalid => "structurally-invalid",
    };
    let cpu_before = profiled_process_ticks();
    let io_before = profiled_server_io();
    let phase_started = Instant::now();
    let mut samples = Vec::with_capacity(repetitions);
    let mut offered_bytes = 0_usize;
    let mut transmitted_bytes = 0_usize;
    for index in 0..repetitions {
        let mut bytes = match kind {
            UploadProfileKind::Valid => test_epub(payload_bytes, 10_000 + index)?,
            UploadProfileKind::Unsupported => vec![b'v'; payload_bytes],
            UploadProfileKind::StructurallyInvalid => {
                let mut bytes = vec![0_u8; payload_bytes.max(4)];
                bytes[..4].copy_from_slice(b"PK\x03\x04");
                bytes
            }
        };
        if !matches!(kind, UploadProfileKind::Valid) {
            let tail = bytes.len() - 1;
            bytes[tail] = bytes[tail].wrapping_add(index as u8);
        }
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let consumed = Arc::new(AtomicUsize::new(0));
        offered_bytes += bytes.len();
        let len = bytes.len();
        let started = Instant::now();
        let response = client
            .put(endpoint(base, &format!("api/libraries/{library_id}/blobs/{hash}"))?)
            .bearer_auth(identity.token.as_ref())
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(streaming_profile_body(Arc::new(bytes), consumed.clone()))
            .send()
            .await?;
        let status = response.status();
        let expected_success = matches!(kind, UploadProfileKind::Valid);
        if status.is_success() != expected_success || (!expected_success && status != reqwest::StatusCode::BAD_REQUEST) {
            return Err(io::Error::other(format!("{label} upload returned unexpected status {status}: {}", response.text().await.unwrap_or_default())).into());
        }
        transmitted_bytes += consumed.load(Ordering::SeqCst);
        samples.push(started.elapsed());
    }
    let wall = phase_started.elapsed();
    let cpu_after = profiled_process_ticks();
    let io_after = profiled_server_io();
    report_latencies(&format!("{label} upload"), &samples, wall);
    println!(
        "{label} upload load: offered={:.1}MiB client_body_polled={:.1}MiB server_cpu_ticks={} postgres_cpu_ticks={} write_bytes={:.1}MiB cancelled_write_bytes={:.1}MiB",
        offered_bytes as f64 / (1024.0 * 1024.0),
        transmitted_bytes as f64 / (1024.0 * 1024.0),
        cpu_after.0.saturating_sub(cpu_before.0),
        cpu_after.1.saturating_sub(cpu_before.1),
        io_after.0.saturating_sub(io_before.0) as f64 / (1024.0 * 1024.0),
        io_after.1.saturating_sub(io_before.1) as f64 / (1024.0 * 1024.0),
    );
    Ok(())
}

async fn run_upload_pipeline_profile(client: &Client, base: &Url, identity: &Identity, payload_bytes: usize, repetitions: usize) -> Result<(), AnyError> {
    let library_id = Uuid::new_v4();
    create_library(client, base, identity, &library_id).await?;
    profile_upload_kind(client, base, identity, &library_id, UploadProfileKind::Valid, payload_bytes, repetitions).await?;
    profile_upload_kind(client, base, identity, &library_id, UploadProfileKind::Unsupported, payload_bytes, repetitions).await?;
    profile_upload_kind(client, base, identity, &library_id, UploadProfileKind::StructurallyInvalid, payload_bytes, repetitions).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let base = loopback_url()?;
    let mode = std::env::var("BOKHEIM_STRESS_MODE").unwrap_or_else(|_| "full".to_owned());
    if !matches!(mode.as_str(), "full" | "metadata" | "position" | "ingest" | "inventory" | "assets" | "uploads" | "register" | "verify" | "token") {
        return Err(io::Error::new(std::io::ErrorKind::InvalidInput, "BOKHEIM_STRESS_MODE must be full, metadata, position, ingest, inventory, assets, uploads, register, verify, or token").into());
    }
    let workload = Workload {
        workers: bounded_env("BOKHEIM_STRESS_WORKERS", 16, 128)?,
        requests_per_worker: bounded_env("BOKHEIM_STRESS_REQUESTS_PER_WORKER", 50, 10_000)?,
        events_per_request: bounded_env("BOKHEIM_STRESS_EVENTS_PER_REQUEST", 4, 128)?,
    };
    let blob_payload_bytes = bounded_env("BOKHEIM_STRESS_BLOB_BYTES", 1024 * 1024, 16 * 1024 * 1024)?;
    let total_events = workload.workers * workload.requests_per_worker * workload.events_per_request;
    println!("Bokheim sync stress: url={base} mode={mode} workers={} requests/worker={} events/request={} events/scenario={total_events}", workload.workers, workload.requests_per_worker, workload.events_per_request);

    let client = Client::builder().pool_max_idle_per_host(workload.workers).timeout(Duration::from_secs(120)).build()?;
    if mode == "register" {
        register(&client, &base).await?;
        return Ok(());
    }
    let identity = match std::env::var("BOKHEIM_STRESS_TOKEN").ok().filter(|token| !token.is_empty()) {
        Some(token) => Identity { token: token.into() },
        None if mode == "verify" => verify_email(&client, &base).await?,
        None => login(&client, &base).await?,
    };
    if matches!(mode.as_str(), "verify" | "token") {
        println!("BOKHEIM_STRESS_TOKEN={}", identity.token);
        return Ok(());
    }
    if mode == "ingest" {
        let book_count = bounded_env("BOKHEIM_INGEST_BOOKS", 1_095, 1_000_000)?;
        let directory_count = bounded_env("BOKHEIM_INGEST_DIRECTORIES", 153, 100_000)?;
        run_library_ingest(&client, &base, &identity, book_count, directory_count).await?;
        println!("stress correctness checks passed for mode=ingest");
        return Ok(());
    }
    if mode == "inventory" {
        let cell_count = bounded_env("BOKHEIM_INVENTORY_CELLS", 10_000, 1_000_000)?;
        let repetitions = bounded_env("BOKHEIM_INVENTORY_REPETITIONS", 5, 100)?;
        run_inventory_profile(&client, &base, &identity, cell_count, repetitions).await?;
        println!("stress correctness checks passed for mode=inventory");
        return Ok(());
    }
    if mode == "assets" {
        let book_count = bounded_env("BOKHEIM_ASSET_PROFILE_BOOKS", 1_095, 4_096)?;
        let repetitions = bounded_env("BOKHEIM_ASSET_PROFILE_REPETITIONS", 3, 100)?;
        run_blob_manifest_profile(&client, &base, &identity, book_count, repetitions).await?;
        println!("stress correctness checks passed for mode=assets");
        return Ok(());
    }
    if mode == "uploads" {
        let payload_bytes = bounded_env("BOKHEIM_UPLOAD_PROFILE_BYTES", 8 * 1024 * 1024, 64 * 1024 * 1024)?;
        let repetitions = bounded_env("BOKHEIM_UPLOAD_PROFILE_REPETITIONS", 5, 100)?;
        run_upload_pipeline_profile(&client, &base, &identity, payload_bytes, repetitions).await?;
        println!("stress correctness checks passed for mode=uploads");
        return Ok(());
    }
    let sharded: Vec<_> = (0..workload.workers).map(|_| Uuid::new_v4()).collect();
    let contended = Uuid::new_v4();
    for library in sharded.iter().chain(std::iter::once(&contended)) {
        create_library(&client, &base, &identity, library).await?;
    }

    if mode != "position" {
        run_pushes("sharded push", &client, &base, &identity, &sharded, workload, 1).await?;
        verify_sharded_pulls(&client, &base, &identity, &sharded, workload.requests_per_worker * workload.events_per_request).await?;
        run_pushes("single-library push", &client, &base, &identity, std::slice::from_ref(&contended), workload, 1_000_000_000).await?;
        run_pull_fanout(&client, &base, &identity, &contended, workload.workers, total_events).await?;
    }

    if mode != "metadata" {
        let position_workload = Workload { events_per_request: 1, ..workload };
        run_pushes("sharded position seed", &client, &base, &identity, &sharded, Workload { requests_per_worker: 1, events_per_request: 1, ..workload }, 2_000_000_000).await?;
        run_position_pushes("sharded reading-position push", &client, &base, &identity, &sharded, position_workload, 2_000_000_000).await?;
        let expected_sharded = if mode == "position" { 2 } else { workload.requests_per_worker * workload.events_per_request + 2 };
        verify_sharded_pulls(&client, &base, &identity, &sharded, expected_sharded).await?;
        run_position_sync_cycles(&client, &base, &identity, &sharded, position_workload, 2_000_000_000).await?;
        run_remote_position_pull_cycles(&client, &base, &identity, &sharded, position_workload, 2_000_000_000).await?;
        run_pushes("single-library position seed", &client, &base, &identity, std::slice::from_ref(&contended), Workload { requests_per_worker: 1, events_per_request: 1, ..workload }, 3_000_000_000).await?;
        run_position_pushes("single-library reading-position push", &client, &base, &identity, std::slice::from_ref(&contended), position_workload, 3_000_000_000).await?;
        let expected_contended_events = if mode == "position" { workload.workers * 2 } else { total_events + workload.workers * 2 };
        let _ = pull_once(&client, &base, &identity, contended, Uuid::new_v4(), expected_contended_events).await?;
        run_replica_convergence_guard(&client, &base, &identity, workload).await?;
    }

    if mode == "full" {
        run_blob_transfers(&client, &base, &identity, &sharded, blob_payload_bytes).await?;
    }

    println!("stress correctness checks passed for mode={mode}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_loopback_url, test_epub};
    use std::collections::HashSet;

    #[test]
    fn stress_target_is_strictly_loopback_http() {
        for accepted in ["http://127.0.0.1:8080", "http://localhost:8080/", "http://[::1]:8080/"] {
            assert!(parse_loopback_url(accepted).is_ok(), "rejected safe target {accepted}");
        }
        for rejected in ["https://127.0.0.1:8080", "http://api.bokheim.se", "http://192.168.1.68:8080", "http://user:password@localhost:8080", "http://localhost:8080/?target=remote"] {
            assert!(parse_loopback_url(rejected).is_err(), "accepted unsafe target {rejected}");
        }
    }

    #[test]
    fn concurrent_epub_fixtures_have_distinct_content_hashes() {
        let hashes: HashSet<_> = (0..128).map(|seed| blake3::hash(&test_epub(1024, seed).unwrap())).collect();
        assert_eq!(hashes.len(), 128);
    }
}
