use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, Method};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, TimeZone, Utc};
use server_postgres::PostgresDatabase;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::Instrument;

const EVENT_CHANNEL_CAPACITY: usize = 8_192;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const RETENTION_DAYS: i64 = 90;
const IP_RETENTION_DAYS: i64 = 7;
const MAX_PENDING_IP_LOGS: usize = 100_000;

#[derive(Clone)]
pub(crate) struct TrafficRecorder {
    sender: mpsc::Sender<TrafficEvent>,
    live: Arc<LiveTraffic>,
    client_address_source: server_account::ClientAddressSource,
}

struct LiveTraffic {
    started_at: Instant,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    dropped_events: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveTrafficSnapshot {
    pub(crate) uptime_seconds: u64,
    pub(crate) in_flight: usize,
    pub(crate) peak_in_flight: usize,
    pub(crate) dropped_events: u64,
}

struct InFlightGuard(Arc<LiveTraffic>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug)]
struct TrafficEvent {
    occurred_at: DateTime<Utc>,
    bucket: DateTime<Utc>,
    client_ip: Option<String>,
    route_group: &'static str,
    method: &'static str,
    status_class: i16,
    request_bytes: i64,
    response_bytes: i64,
    duration_ms: i64,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct AggregateKey {
    bucket: DateTime<Utc>,
    route_group: &'static str,
    method: &'static str,
    status_class: i16,
}

#[derive(Clone, Copy, Debug, Default)]
struct Aggregate {
    request_count: i64,
    request_bytes: i64,
    response_bytes: i64,
    total_duration_ms: i64,
    max_duration_ms: i64,
}

impl Aggregate {
    fn add_event(&mut self, event: &TrafficEvent) {
        self.request_count = self.request_count.saturating_add(1);
        self.request_bytes = self.request_bytes.saturating_add(event.request_bytes);
        self.response_bytes = self.response_bytes.saturating_add(event.response_bytes);
        self.total_duration_ms = self.total_duration_ms.saturating_add(event.duration_ms);
        self.max_duration_ms = self.max_duration_ms.max(event.duration_ms);
    }

    fn merge(&mut self, other: Self) {
        self.request_count = self.request_count.saturating_add(other.request_count);
        self.request_bytes = self.request_bytes.saturating_add(other.request_bytes);
        self.response_bytes = self.response_bytes.saturating_add(other.response_bytes);
        self.total_duration_ms = self.total_duration_ms.saturating_add(other.total_duration_ms);
        self.max_duration_ms = self.max_duration_ms.max(other.max_duration_ms);
    }
}

impl TrafficRecorder {
    pub(crate) fn new(database: PostgresDatabase, client_address_source: server_account::ClientAddressSource) -> Self {
        let (sender, receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let recorder = Self { sender, live: Arc::new(LiveTraffic { started_at: Instant::now(), in_flight: AtomicUsize::new(0), peak_in_flight: AtomicUsize::new(0), dropped_events: AtomicU64::new(0) }), client_address_source };
        tokio::spawn(run_aggregator(database, receiver, recorder.live.clone()));
        recorder
    }

    pub(crate) fn snapshot(&self) -> LiveTrafficSnapshot {
        LiveTrafficSnapshot {
            uptime_seconds: self.live.started_at.elapsed().as_secs(),
            in_flight: self.live.in_flight.load(Ordering::Relaxed),
            peak_in_flight: self.live.peak_in_flight.load(Ordering::Relaxed),
            dropped_events: self.live.dropped_events.load(Ordering::Relaxed),
        }
    }

    fn begin_request(&self) -> InFlightGuard {
        let current = self.live.in_flight.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        self.live.peak_in_flight.fetch_max(current, Ordering::Relaxed);
        InFlightGuard(self.live.clone())
    }

    fn record(&self, event: TrafficEvent) {
        if self.sender.try_send(event).is_err() {
            self.live.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) async fn record_traffic(State(recorder): State<TrafficRecorder>, request: Request, next: Next) -> Response {
    let _in_flight = recorder.begin_request();
    let started = Instant::now();
    let route_group = route_group(request.uri().path());
    let method = method_group(request.method());
    let request_bytes = content_length(request.headers());
    let remote = request.extensions().get::<ConnectInfo<SocketAddr>>().cloned();
    let client_ip = server_account::http::client_address(recorder.client_address_source, request.headers(), remote).ok().map(|address| address.to_string());
    let trace_id = request.headers().get("x-bokheim-trace-id").and_then(|value| value.to_str().ok()).and_then(|value| uuid::Uuid::parse_str(value).ok()).unwrap_or_else(uuid::Uuid::new_v4);
    let span = tracing::debug_span!(target: "sync_performance", "server_request", %trace_id, route_group, method, request_bytes);
    let response = async {
        tracing::debug!(target: "sync_performance", event = "start", "request started");
        let response = next.run(request).await;
        tracing::debug!(target: "sync_performance", event = "complete", status = response.status().as_u16(), response_bytes = content_length(response.headers()), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, "request headers ready");
        response
    }.instrument(span).await;
    let occurred_at = Utc::now();
    let event = TrafficEvent {
        occurred_at,
        bucket: minute_bucket(occurred_at),
        client_ip,
        route_group,
        method,
        status_class: i16::try_from(response.status().as_u16() / 100).unwrap_or(5).clamp(1, 5),
        request_bytes,
        response_bytes: content_length(response.headers()),
        duration_ms: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
    };
    recorder.record(event);
    response
}

fn minute_bucket(value: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_opt(value.timestamp().div_euclid(60) * 60, 0).single().unwrap_or(value)
}

fn content_length(headers: &axum::http::HeaderMap) -> i64 {
    headers.get(header::CONTENT_LENGTH).and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok()).map(|value| i64::try_from(value).unwrap_or(i64::MAX)).unwrap_or(0)
}

fn method_group(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

fn route_group(path: &str) -> &'static str {
    if path == "/api/live" || path == "/api/ready" || path == "/api/health" {
        "/api/health"
    } else if path.starts_with("/api/admin/") {
        "/api/admin"
    } else if path.starts_with("/api/auth/") {
        "/api/auth"
    } else if path == "/api/notifications" {
        "/api/notifications"
    } else if path.starts_with("/api/sync/") {
        "/api/sync"
    } else if path.contains("/thumbnail") {
        "/api/thumbnails"
    } else if path.contains("/blobs/") || path.ends_with("/book-batch") {
        "/api/blobs"
    } else if path.starts_with("/api/libraries") {
        "/api/libraries"
    } else if path.starts_with("/api/account/") {
        "/api/account"
    } else {
        "/other"
    }
}

async fn run_aggregator(database: PostgresDatabase, mut receiver: mpsc::Receiver<TrafficEvent>, live: Arc<LiveTraffic>) {
    let mut pending: HashMap<AggregateKey, Aggregate> = HashMap::new();
    let mut pending_ip_logs = Vec::new();
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    flush.tick().await;
    let mut cleanup = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
    cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Some(event) => {
                    let key = AggregateKey { bucket: event.bucket, route_group: event.route_group, method: event.method, status_class: event.status_class };
                    pending.entry(key).or_default().add_event(&event);
                    if event.client_ip.is_some() {
                        if pending_ip_logs.len() < MAX_PENDING_IP_LOGS {
                            pending_ip_logs.push(event);
                        } else {
                            live.dropped_events.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                None => {
                    flush_pending(&database, &mut pending, &mut pending_ip_logs).await;
                    break;
                }
            },
            _ = flush.tick() => {
                flush_pending(&database, &mut pending, &mut pending_ip_logs).await;
            },
            _ = cleanup.tick() => {
                if let Err(error) = sqlx::query("DELETE FROM admin_traffic_minute WHERE bucket < now() - ($1 * INTERVAL '1 day')").bind(RETENTION_DAYS).execute(database.pool()).await {
                    tracing::warn!(%error, "failed to prune administrator traffic history");
                }
                if let Err(error) = sqlx::query("DELETE FROM admin_request_ip WHERE occurred_at < now() - ($1 * INTERVAL '1 day')").bind(IP_RETENTION_DAYS).execute(database.pool()).await {
                    tracing::warn!(%error, "failed to prune administrator request IP history");
                }
            }
        }
    }
}

async fn flush_pending(database: &PostgresDatabase, pending: &mut HashMap<AggregateKey, Aggregate>, pending_ip_logs: &mut Vec<TrafficEvent>) {
    if pending.is_empty() && pending_ip_logs.is_empty() {
        return;
    }
    let batch = std::mem::take(pending);
    let ip_batch = std::mem::take(pending_ip_logs);
    let mut transaction = match database.pool().begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::warn!(%error, "failed to begin administrator traffic flush");
            merge_batch(pending, batch);
            pending_ip_logs.extend(ip_batch);
            return;
        }
    };
    let mut failed = false;
    for (key, value) in &batch {
        let result = sqlx::query(
            "INSERT INTO admin_traffic_minute
             (bucket, route_group, method, status_class, request_count, request_bytes, response_bytes, total_duration_ms, max_duration_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (bucket, route_group, method, status_class) DO UPDATE SET
                request_count=admin_traffic_minute.request_count + EXCLUDED.request_count,
                request_bytes=admin_traffic_minute.request_bytes + EXCLUDED.request_bytes,
                response_bytes=admin_traffic_minute.response_bytes + EXCLUDED.response_bytes,
                total_duration_ms=admin_traffic_minute.total_duration_ms + EXCLUDED.total_duration_ms,
                max_duration_ms=GREATEST(admin_traffic_minute.max_duration_ms, EXCLUDED.max_duration_ms)",
        )
        .bind(key.bucket)
        .bind(key.route_group)
        .bind(key.method)
        .bind(key.status_class)
        .bind(value.request_count)
        .bind(value.request_bytes)
        .bind(value.response_bytes)
        .bind(value.total_duration_ms)
        .bind(value.max_duration_ms)
        .execute(&mut *transaction)
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "failed to aggregate administrator traffic history");
            failed = true;
            break;
        }
    }
    if !failed && !ip_batch.is_empty() {
        let occurred_at = ip_batch.iter().map(|event| event.occurred_at).collect::<Vec<_>>();
        let client_ip = ip_batch.iter().map(|event| event.client_ip.clone().expect("IP batches contain addresses")).collect::<Vec<_>>();
        let route_group = ip_batch.iter().map(|event| event.route_group.to_owned()).collect::<Vec<_>>();
        let method = ip_batch.iter().map(|event| event.method.to_owned()).collect::<Vec<_>>();
        let status_class = ip_batch.iter().map(|event| event.status_class).collect::<Vec<_>>();
        let request_bytes = ip_batch.iter().map(|event| event.request_bytes).collect::<Vec<_>>();
        let response_bytes = ip_batch.iter().map(|event| event.response_bytes).collect::<Vec<_>>();
        let duration_ms = ip_batch.iter().map(|event| event.duration_ms).collect::<Vec<_>>();
        if let Err(error) = sqlx::query(
            "INSERT INTO admin_request_ip(occurred_at, client_ip, route_group, method, status_class, request_bytes, response_bytes, duration_ms)
             SELECT rows.occurred_at, rows.client_ip::inet, rows.route_group, rows.method, rows.status_class, rows.request_bytes, rows.response_bytes, rows.duration_ms
             FROM UNNEST($1::timestamptz[], $2::text[], $3::text[], $4::text[], $5::smallint[], $6::bigint[], $7::bigint[], $8::bigint[])
             AS rows(occurred_at, client_ip, route_group, method, status_class, request_bytes, response_bytes, duration_ms)",
        )
        .bind(occurred_at)
        .bind(client_ip)
        .bind(route_group)
        .bind(method)
        .bind(status_class)
        .bind(request_bytes)
        .bind(response_bytes)
        .bind(duration_ms)
        .execute(&mut *transaction)
        .await
        {
            tracing::warn!(%error, "failed to store administrator request IP history");
            failed = true;
        }
    }
    if failed {
        drop(transaction);
        merge_batch(pending, batch);
        pending_ip_logs.extend(ip_batch);
        return;
    }
    if let Err(error) = transaction.commit().await {
        tracing::warn!(%error, "failed to commit administrator traffic history");
        merge_batch(pending, batch);
        pending_ip_logs.extend(ip_batch);
    }
}

fn merge_batch(pending: &mut HashMap<AggregateKey, Aggregate>, batch: HashMap<AggregateKey, Aggregate>) {
    for (key, value) in batch {
        pending.entry(key).or_default().merge(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_reduced_to_bounded_non_sensitive_categories() {
        assert_eq!(route_group("/api/libraries/private-library/blobs/private-hash"), "/api/blobs");
        assert_eq!(route_group("/api/libraries/private-library/book-batch"), "/api/blobs");
        assert_eq!(route_group("/api/libraries/private-library/thumbnails/private-hash"), "/api/thumbnails");
        assert_eq!(route_group("/api/auth/login"), "/api/auth");
        assert_eq!(route_group("/unexpected/private-value"), "/other");
    }

    #[test]
    fn aggregates_saturate_and_preserve_maximum_latency() {
        let mut total = Aggregate::default();
        total.merge(Aggregate { request_count: 2, request_bytes: 10, response_bytes: 20, total_duration_ms: 30, max_duration_ms: 20 });
        total.merge(Aggregate { request_count: 1, request_bytes: 5, response_bytes: 7, total_duration_ms: 50, max_duration_ms: 50 });
        assert_eq!(total.request_count, 3);
        assert_eq!(total.request_bytes, 15);
        assert_eq!(total.response_bytes, 27);
        assert_eq!(total.total_duration_ms, 80);
        assert_eq!(total.max_duration_ms, 50);
    }
}
