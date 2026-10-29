//! Device-wide admission and cancellation for automatic file transfers.
//! Reading and explicit user downloads bypass this gate.
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::OnceLock;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum NetworkCost {
    #[default]
    Unknown,
    Unmetered,
    Metered,
}

pub struct TransferPolicy {
    allow_metered: AtomicBool,
    allow_seeding: AtomicBool,
    cost: AtomicU8,
    changes: OnceLock<watch::Sender<()>>,
}

impl Default for TransferPolicy {
    fn default() -> Self {
        Self { allow_metered: AtomicBool::new(false), allow_seeding: AtomicBool::new(true), cost: AtomicU8::new(0), changes: OnceLock::new() }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Deferred;

impl std::fmt::Display for Deferred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waiting for an unmetered connection")
    }
}
impl std::error::Error for Deferred {}

impl TransferPolicy {
    /// Device power policy for completed torrents, independent of downloads.
    pub fn set_allow_seeding(&self, allow: bool) {
        if self.allow_seeding.swap(allow, Ordering::AcqRel) != allow { self.notify(); }
    }
    pub fn seeding_allowed(&self) -> bool {
        self.allow_seeding.load(Ordering::Acquire) && self.allowed()
    }
    pub fn set_allow_metered(&self, allow: bool) {
        if self.allow_metered.swap(allow, Ordering::AcqRel) != allow {
            self.notify();
        }
    }
    pub fn set_network_cost(&self, cost: NetworkCost) {
        if self.cost.swap(cost as u8, Ordering::AcqRel) != cost as u8 {
            self.notify();
        }
    }
    fn notify(&self) {
        self.changes.get_or_init(|| watch::channel(()).0).send_replace(());
    }
    /// Subscribe before checking admission so a concurrent change cannot be lost.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.get_or_init(|| watch::channel(()).0).subscribe()
    }
    async fn wait_until(&self, allowed: bool) {
        let mut changes = self.subscribe();
        while self.allowed() != allowed {
            changes.changed().await.expect("policy owns its notification sender");
        }
    }
    pub fn allowed(&self) -> bool {
        self.allow_metered.load(Ordering::Acquire) || self.cost.load(Ordering::Acquire) < NetworkCost::Metered as u8
    }
    pub async fn wait(&self) {
        self.wait_until(true).await;
    }
    /// Cancel a request if the connection becomes metered. Its durable owner
    /// retries it, keeping the original queue entry and publication intent.
    pub async fn run<T>(&self, request: impl Future<Output = T>) -> Result<T, Deferred> {
        self.wait().await;
        self.try_run(request).await
    }
    /// Leave deferred work with its queue instead of occupying a worker while
    /// waiting. This lets later user-initiated jobs bypass the restriction.
    pub async fn try_run<T>(&self, request: impl Future<Output = T>) -> Result<T, Deferred> {
        if !self.allowed() {
            return Err(Deferred);
        }
        tokio::select! {
            biased;
            _ = self.wait_until(false) => Err(Deferred),
            result = request => if self.allowed() { Ok(result) } else { Err(Deferred) },
        }
    }
    /// Fetches without a durable queue restart from their original input.
    pub async fn retry<T, F: Future<Output = T>>(&self, mut request: impl FnMut() -> F) -> T {
        loop {
            if let Ok(result) = self.run(request()).await {
                return result;
            }
        }
    }
}

pub static POLICY: TransferPolicy = TransferPolicy { allow_metered: AtomicBool::new(false), allow_seeding: AtomicBool::new(true), cost: AtomicU8::new(0), changes: OnceLock::new() };
pub fn allowed() -> bool {
    POLICY.allowed()
}
pub async fn wait() {
    POLICY.wait().await;
}
pub async fn run<T>(request: impl Future<Output = T>) -> Result<T, Deferred> {
    POLICY.run(request).await
}
pub async fn try_run<T>(request: impl Future<Output = T>) -> Result<T, Deferred> {
    POLICY.try_run(request).await
}
pub async fn retry<T, F: Future<Output = T>>(request: impl FnMut() -> F) -> T {
    POLICY.retry(request).await
}

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{configure_reader, initialize, initialize_device, save_allow_metered};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::sleep;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[test]
    fn disabling_seeding_keeps_downloads_available_even_with_metered_override() {
        let policy = TransferPolicy::default();
        assert!(policy.seeding_allowed());
        policy.set_allow_seeding(false);
        assert!(policy.allowed());
        assert!(!policy.seeding_allowed());
        policy.set_allow_metered(true);
        assert!(policy.allowed());
        assert!(!policy.seeding_allowed());
    }

    #[tokio::test]
    async fn changes_notify_all_subscribers_without_repeating_identical_observations() {
        let policy = TransferPolicy::default();
        let mut first = policy.subscribe();
        let mut second = policy.subscribe();
        policy.set_network_cost(NetworkCost::Metered);
        first.changed().await.unwrap();
        second.changed().await.unwrap();
        policy.set_network_cost(NetworkCost::Metered);
        policy.set_allow_metered(false);
        assert!(!first.has_changed().unwrap());
        policy.set_allow_metered(true);
        first.changed().await.unwrap();
        second.changed().await.unwrap();
        assert!(policy.allowed());
    }

    #[tokio::test]
    async fn deferred_queue_work_releases_the_worker_without_polling_the_request() {
        let policy = TransferPolicy::default();
        policy.set_network_cost(NetworkCost::Metered);
        assert!(policy.try_run(async { panic!("deferred requests must not start") }).await.is_err());
        policy.set_network_cost(NetworkCost::Unmetered);
        assert_eq!(policy.try_run(async { 42 }).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn metered_admission_waits_and_override_releases_it() {
        let policy = Arc::new(TransferPolicy::default());
        policy.set_network_cost(NetworkCost::Metered);
        let (started, mut received) = oneshot::channel();
        let worker = tokio::spawn({
            let policy = policy.clone();
            async move {
                policy
                    .run(async {
                        started.send(()).unwrap();
                    })
                    .await
            }
        });
        sleep(Duration::from_millis(300)).await.unwrap();
        assert!(received.try_recv().is_err());
        policy.set_allow_metered(true);
        worker.await.unwrap().unwrap();
        received.await.unwrap();
    }

    #[tokio::test]
    async fn changing_network_cancels_and_retries_owned_requests() {
        struct Owned(Arc<AtomicBool>);
        impl Drop for Owned {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let policy = Arc::new(TransferPolicy::default());
        policy.set_network_cost(NetworkCost::Unmetered);
        let dropped = Arc::new(AtomicBool::new(false));
        let (started, received) = oneshot::channel();
        let worker = tokio::spawn({
            let policy = policy.clone();
            let dropped = dropped.clone();
            async move {
                policy
                    .run(async {
                        let _owned = Owned(dropped);
                        started.send(()).unwrap();
                        std::future::pending::<()>().await
                    })
                    .await
            }
        });
        received.await.unwrap();
        policy.set_network_cost(NetworkCost::Metered);
        assert!(tokio::time::timeout(Duration::from_millis(100), worker).await.expect("network change must cancel without a polling interval").unwrap().is_err());
        assert!(dropped.load(Ordering::Acquire));
        let resumed = tokio::spawn({
            let policy = policy.clone();
            async move { policy.retry(|| async { 42 }).await }
        });
        sleep(Duration::from_millis(300)).await.unwrap();
        assert!(!resumed.is_finished());
        policy.set_network_cost(NetworkCost::Unmetered);
        assert_eq!(resumed.await.unwrap(), 42);
        policy.set_network_cost(NetworkCost::Unknown);
        assert!(policy.allowed(), "missing OS cost information is not a permanent block");
    }
}
