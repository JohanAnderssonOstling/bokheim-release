use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior};

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const FAIR_QUANTUM_BYTES: usize = 64 * 1024;
const SCHEDULER_TICK: Duration = Duration::from_millis(10);
const MAX_PENDING_GRANTS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferLimits {
    pub upload_bytes_per_second: u64,
    pub download_bytes_per_second: u64,
    pub burst_bytes: u64,
}

impl Default for TransferLimits {
    fn default() -> Self {
        Self { upload_bytes_per_second: 100 * 1024 * 1024, download_bytes_per_second: 100 * 1024 * 1024, burst_bytes: 1024 * 1024 }
    }
}

impl TransferLimits {
    pub fn from_env() -> Result<Self, String> {
        let defaults = Self::default();
        Ok(Self {
            upload_bytes_per_second: parse_env_u64("BOKHEIM_UPLOAD_BYTES_PER_SECOND", defaults.upload_bytes_per_second)?,
            download_bytes_per_second: parse_env_u64("BOKHEIM_DOWNLOAD_BYTES_PER_SECOND", defaults.download_bytes_per_second)?,
            burst_bytes: parse_env_u64("BOKHEIM_TRANSFER_BURST_BYTES", defaults.burst_bytes)?.max(FAIR_QUANTUM_BYTES as u64),
        })
    }
}

fn parse_env_u64(name: &str, default: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(raw) => raw.parse::<u64>().map_err(|_| format!("{name} must be an unsigned byte count")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(format!("cannot read {name}: {error}")),
    }
}

#[derive(Clone)]
pub(crate) struct TransferGovernor {
    upload: DirectionGovernor,
    download: DirectionGovernor,
}

impl TransferGovernor {
    pub(crate) fn new(limits: TransferLimits) -> Self {
        Self { upload: DirectionGovernor::new(limits.upload_bytes_per_second, limits.burst_bytes), download: DirectionGovernor::new(limits.download_bytes_per_second, limits.burst_bytes) }
    }

    pub(crate) async fn upload(&self, user_id: &str, bytes: usize) -> std::io::Result<()> {
        self.upload.acquire(user_id, bytes).await
    }

    pub(crate) async fn download(&self, user_id: &str, bytes: usize) -> std::io::Result<()> {
        self.download.acquire(user_id, bytes).await
    }
}

#[derive(Clone)]
struct DirectionGovernor {
    inner: std::sync::Arc<DirectionInner>,
}

struct DirectionInner {
    sender: mpsc::Sender<GrantRequest>,
    receiver: Mutex<Option<mpsc::Receiver<GrantRequest>>>,
    started: AtomicBool,
    rate_bytes_per_second: u64,
    burst_bytes: u64,
}

impl DirectionGovernor {
    fn new(rate_bytes_per_second: u64, burst_bytes: u64) -> Self {
        let (sender, receiver) = mpsc::channel(MAX_PENDING_GRANTS);
        Self { inner: std::sync::Arc::new(DirectionInner { sender, receiver: Mutex::new(Some(receiver)), started: AtomicBool::new(false), rate_bytes_per_second, burst_bytes: burst_bytes.max(FAIR_QUANTUM_BYTES as u64) }) }
    }

    fn ensure_started(&self) -> std::io::Result<()> {
        if self.inner.rate_bytes_per_second == 0 || self.inner.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let receiver = self.inner.receiver.lock().map_err(|_| std::io::Error::other("bandwidth scheduler lock is poisoned"))?.take().ok_or_else(|| std::io::Error::other("bandwidth scheduler receiver is unavailable"))?;
        let rate = self.inner.rate_bytes_per_second;
        let burst = self.inner.burst_bytes;
        tokio::spawn(async move { run_scheduler(receiver, rate, burst).await });
        Ok(())
    }

    async fn acquire(&self, user_id: &str, bytes: usize) -> std::io::Result<()> {
        if bytes == 0 || self.inner.rate_bytes_per_second == 0 {
            return Ok(());
        }
        self.ensure_started()?;
        let mut remaining = bytes;
        while remaining > 0 {
            let chunk = remaining.min(FAIR_QUANTUM_BYTES);
            let (granted, reply) = oneshot::channel();
            self.inner.sender.send(GrantRequest { user_id: user_id.to_string(), bytes: chunk, granted }).await.map_err(|_| std::io::Error::other("bandwidth scheduler stopped"))?;
            reply.await.map_err(|_| std::io::Error::other("bandwidth scheduler dropped a grant"))?;
            remaining -= chunk;
        }
        Ok(())
    }
}

struct GrantRequest {
    user_id: String,
    bytes: usize,
    granted: oneshot::Sender<()>,
}

struct SchedulerState {
    queues: HashMap<String, VecDeque<GrantRequest>>,
    active_users: VecDeque<String>,
    deficits: HashMap<String, usize>,
    credit: u128,
    credit_cap: u128,
}

impl SchedulerState {
    fn new(burst_bytes: u64) -> Self {
        let credit_cap = u128::from(burst_bytes) * NANOS_PER_SECOND;
        Self { queues: HashMap::new(), active_users: VecDeque::new(), deficits: HashMap::new(), credit: credit_cap, credit_cap }
    }

    fn enqueue(&mut self, request: GrantRequest) {
        let user_id = request.user_id.clone();
        let queue = self.queues.entry(user_id.clone()).or_default();
        let was_empty = queue.is_empty();
        queue.push_back(request);
        if was_empty {
            self.active_users.push_back(user_id.clone());
            self.deficits.entry(user_id).or_default();
        }
    }

    fn refill(&mut self, elapsed: Duration, rate_bytes_per_second: u64) {
        let added = elapsed.as_nanos().saturating_mul(u128::from(rate_bytes_per_second));
        self.credit = self.credit.saturating_add(added).min(self.credit_cap);
    }

    fn grant_ready(&mut self) {
        while !self.active_users.is_empty() {
            let users_this_round = self.active_users.len();
            let mut granted_in_round = false;
            for _ in 0..users_this_round {
                let Some(user_id) = self.active_users.pop_front() else {
                    break;
                };
                let deficit = self.deficits.entry(user_id.clone()).or_default();
                *deficit = deficit.saturating_add(FAIR_QUANTUM_BYTES);
                let queue = self.queues.get_mut(&user_id).expect("active user must have a queue");
                while let Some(request) = queue.front() {
                    if request.granted.is_closed() {
                        queue.pop_front();
                        continue;
                    }
                    let cost = (request.bytes as u128) * NANOS_PER_SECOND;
                    if request.bytes > *deficit || cost > self.credit {
                        break;
                    }
                    let request = queue.pop_front().expect("front request exists");
                    *deficit -= request.bytes;
                    self.credit -= cost;
                    let _ = request.granted.send(());
                    granted_in_round = true;
                }
                if queue.is_empty() {
                    self.queues.remove(&user_id);
                    self.deficits.remove(&user_id);
                } else {
                    self.active_users.push_back(user_id);
                }
            }
            if !granted_in_round {
                break;
            }
        }
    }
}

async fn run_scheduler(mut receiver: mpsc::Receiver<GrantRequest>, rate_bytes_per_second: u64, burst_bytes: u64) {
    let mut state = SchedulerState::new(burst_bytes);
    let mut interval = tokio::time::interval(SCHEDULER_TICK);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_refill = Instant::now();
    loop {
        tokio::select! {
            request = receiver.recv() => {
                let Some(request) = request else { break };
                let now = Instant::now();
                state.refill(now.saturating_duration_since(last_refill), rate_bytes_per_second);
                last_refill = now;
                state.enqueue(request);
                state.grant_ready();
            }
            _ = interval.tick() => {
                let now = Instant::now();
                state.refill(now.saturating_duration_since(last_refill), rate_bytes_per_second);
                last_refill = now;
                state.grant_ready();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(user_id: &str, bytes: usize) -> (GrantRequest, oneshot::Receiver<()>) {
        let (granted, receiver) = oneshot::channel();
        (GrantRequest { user_id: user_id.to_string(), bytes, granted }, receiver)
    }

    #[test]
    fn deficit_round_robin_is_fair_per_user_not_per_connection() {
        let mut state = SchedulerState::new((2 * FAIR_QUANTUM_BYTES) as u64);
        let mut alpha = Vec::new();
        for _ in 0..4 {
            let (request, granted) = request("alpha", FAIR_QUANTUM_BYTES);
            state.enqueue(request);
            alpha.push(granted);
        }
        let (request, mut beta) = request("beta", FAIR_QUANTUM_BYTES);
        state.enqueue(request);

        state.grant_ready();

        assert!(alpha[0].try_recv().is_ok());
        assert!(beta.try_recv().is_ok(), "the second user receives the second quantum even when the first opened four streams");
        assert!(alpha[1].try_recv().is_err());
    }

    #[test]
    fn an_uncontended_user_can_consume_the_complete_available_capacity() {
        let mut state = SchedulerState::new((3 * FAIR_QUANTUM_BYTES) as u64);
        let mut grants = Vec::new();
        for _ in 0..3 {
            let (request, granted) = request("only-user", FAIR_QUANTUM_BYTES);
            state.enqueue(request);
            grants.push(granted);
        }
        state.grant_ready();
        assert!(grants.iter_mut().all(|grant| grant.try_recv().is_ok()));
    }

    #[test]
    fn exhausted_burst_is_replenished_at_the_configured_byte_rate() {
        let mut state = SchedulerState::new(FAIR_QUANTUM_BYTES as u64);
        let (first, mut first_grant) = request("user", FAIR_QUANTUM_BYTES);
        let (second, mut second_grant) = request("user", FAIR_QUANTUM_BYTES);
        state.enqueue(first);
        state.enqueue(second);
        state.grant_ready();
        assert!(first_grant.try_recv().is_ok());
        assert!(second_grant.try_recv().is_err());

        state.refill(Duration::from_millis(500), (2 * FAIR_QUANTUM_BYTES) as u64);
        state.grant_ready();
        assert!(second_grant.try_recv().is_ok());
    }

    #[test]
    fn zero_rate_explicitly_disables_throttling() {
        let governor = DirectionGovernor::new(0, FAIR_QUANTUM_BYTES as u64);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(governor.acquire("user", usize::MAX)).unwrap();
    }
}
