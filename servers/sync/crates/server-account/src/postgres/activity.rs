use chrono::{NaiveDate, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

const CHANNEL_CAPACITY: usize = 8_192;
const MAX_PENDING_ACCOUNTS: usize = 100_000;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const DAILY_RETENTION_DAYS: i64 = 90;

pub(super) struct AccountActivityRecorder {
    sender: mpsc::Sender<ActivityEvent>,
}

struct ActivityEvent {
    user_id: String,
    activity_date: NaiveDate,
    kind: ActivityKind,
}

enum ActivityKind {
    Authenticated,
    Sync,
}

#[derive(Clone, Copy, Default)]
struct ActivityCounts {
    requests: i64,
    sync_requests: i64,
}

impl AccountActivityRecorder {
    pub(super) fn new(pool: PgPool) -> Self {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        tokio::spawn(run(pool, receiver));
        Self { sender }
    }

    pub(super) fn record_authenticated(&self, user_id: &str) {
        self.record(user_id, ActivityKind::Authenticated);
    }

    pub(super) fn record_sync(&self, user_id: &str) {
        self.record(user_id, ActivityKind::Sync);
    }

    fn record(&self, user_id: &str, kind: ActivityKind) {
        let _ = self.sender.try_send(ActivityEvent { user_id: user_id.to_owned(), activity_date: Utc::now().date_naive(), kind });
    }
}

async fn run(pool: PgPool, mut receiver: mpsc::Receiver<ActivityEvent>) {
    let mut pending = HashMap::<(String, NaiveDate), ActivityCounts>::new();
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    let mut cleanup = tokio::time::interval(CLEANUP_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    flush.tick().await;
    cleanup.tick().await;
    cleanup_old_daily_activity(&pool).await;
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Some(event) => {
                    let key = (event.user_id, event.activity_date);
                    if pending.len() < MAX_PENDING_ACCOUNTS || pending.contains_key(&key) {
                        let counts = pending.entry(key).or_default();
                        match event.kind {
                            ActivityKind::Authenticated => counts.requests = counts.requests.saturating_add(1),
                            ActivityKind::Sync => counts.sync_requests = counts.sync_requests.saturating_add(1),
                        }
                    }
                }
                None => {
                    flush_pending(&pool, &mut pending).await;
                    break;
                }
            },
            _ = flush.tick() => flush_pending(&pool, &mut pending).await,
            _ = cleanup.tick() => cleanup_old_daily_activity(&pool).await,
        }
    }
}

async fn cleanup_old_daily_activity(pool: &PgPool) {
    if let Err(error) = sqlx::query("DELETE FROM admin_account_activity_day WHERE activity_date < CURRENT_DATE - ($1::integer - 1)").bind(DAILY_RETENTION_DAYS).execute(pool).await {
        tracing::warn!(%error, "failed to expire account engagement history");
    }
}

async fn flush_pending(pool: &PgPool, pending: &mut HashMap<(String, NaiveDate), ActivityCounts>) {
    if pending.is_empty() {
        return;
    }
    let batch = std::mem::take(pending);
    let keys = batch.keys().cloned().collect::<Vec<_>>();
    let user_ids = keys.iter().map(|key| key.0.clone()).collect::<Vec<_>>();
    let activity_dates = keys.iter().map(|key| key.1).collect::<Vec<_>>();
    let request_counts = keys.iter().map(|key| batch[key].requests).collect::<Vec<_>>();
    let sync_request_counts = keys.iter().map(|key| batch[key].sync_requests).collect::<Vec<_>>();
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            restore_batch(pending, batch);
            tracing::warn!(%error, "failed to begin account engagement flush");
            return;
        }
    };
    let account_result = sqlx::query(
        "INSERT INTO admin_account_activity(user_id, first_seen_at, last_seen_at, request_count)
         SELECT input.user_id, now(), now(), input.request_count
         FROM UNNEST($1::text[], $2::bigint[]) AS input(user_id, request_count)
         JOIN users ON users.id = input.user_id
         WHERE input.request_count > 0
         ON CONFLICT (user_id) DO UPDATE SET
            last_seen_at=EXCLUDED.last_seen_at,
            request_count=admin_account_activity.request_count + EXCLUDED.request_count",
    )
    .bind(&user_ids)
    .bind(&request_counts)
    .execute(&mut *transaction)
    .await;
    let daily_result = async {
        account_result?;
        sqlx::query(
            "INSERT INTO admin_account_activity_day(user_id, activity_date, request_count, sync_request_count)
             SELECT input.user_id, input.activity_date, input.request_count, input.sync_request_count
             FROM UNNEST($1::text[], $2::date[], $3::bigint[], $4::bigint[])
                  AS input(user_id, activity_date, request_count, sync_request_count)
             JOIN users ON users.id = input.user_id
             WHERE input.request_count + input.sync_request_count > 0
             ON CONFLICT (user_id, activity_date) DO UPDATE SET
                request_count=admin_account_activity_day.request_count + EXCLUDED.request_count,
                sync_request_count=admin_account_activity_day.sync_request_count + EXCLUDED.sync_request_count",
        )
        .bind(&user_ids)
        .bind(&activity_dates)
        .bind(&request_counts)
        .bind(&sync_request_counts)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    }
    .await;
    if let Err(error) = daily_result {
        restore_batch(pending, batch);
        tracing::warn!(%error, "failed to flush account engagement activity");
    }
}

fn restore_batch(pending: &mut HashMap<(String, NaiveDate), ActivityCounts>, batch: HashMap<(String, NaiveDate), ActivityCounts>) {
    for (key, counts) in batch {
        let pending_counts = pending.entry(key).or_default();
        pending_counts.requests = pending_counts.requests.saturating_add(counts.requests);
        pending_counts.sync_requests = pending_counts.sync_requests.saturating_add(counts.sync_requests);
    }
}
