//! Account-scoped wakeups. Reconnect reconciles missed events; no remote polling.
use crate::sync::SyncScheduler;
use crate::DeviceAccountSession;
use std::time::Duration;
use sync_common::api::notifications::NotificationMessage;
use web_time::Instant;

/// Notification reconnects need a foreground-resume throttle, not a second
/// sync lifecycle. It belongs to the socket owner.
struct ResumeRefresh {
    foreground: bool,
    generation: u64,
    refreshed: Instant,
}

impl ResumeRefresh {
    fn new(foreground: bool, generation: u64, now: Instant) -> Self {
        Self { foreground, generation, refreshed: now }
    }

    fn observe(&mut self, foreground: bool, generation: u64, now: Instant) -> bool {
        let resumed = foreground && (!self.foreground || generation != self.generation);
        self.foreground = foreground;
        self.generation = generation;
        if resumed && now.duration_since(self.refreshed) >= Duration::from_secs(30) {
            self.refreshed = now;
            true
        } else {
            false
        }
    }
}

pub(super) async fn run(account: DeviceAccountSession, discovery: SyncScheduler, interest: crate::notification_interest::NotificationInterest, remote_changes: crate::notification_interest::RemoteChanges) {
    let mut activity = interest.subscribe();
    let initial_activity = *activity.borrow_and_update();
    let mut resume = ResumeRefresh::new(initial_activity.foreground(), initial_activity.resume_generation(), Instant::now());
    // Token refresh also resumes persisted work in other libraries, even when
    // the notification socket is inactive on the reader screen.
    let mut changed = account.subscribe();
    let mut generation = *changed.borrow_and_update();
    let mut retry = 1;
    loop {
        let current_generation = *changed.borrow_and_update();
        if current_generation != generation {
            generation = current_generation;
            discovery.request_remote_refresh();
        }
        let policy = *activity.borrow_and_update();
        if resume.observe(policy.foreground(), policy.resume_generation(), Instant::now()) {
            // Resume does not depend on a successful WebSocket handshake.
            if account.current().ok().flatten().is_some() {
                remote_changes.request();
                discovery.request_remote_refresh();
            }
        }
        let attempt = async {
            if !policy.enabled() {
                return std::future::pending().await;
            }
            let session = account.refresh_if_needed().await.map_err(|e| e.to_string())?;
            let Some(session) = session else { return std::future::pending().await };
            let mut url = session.server_url().endpoint("api/notifications");
            let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
            url.set_scheme(scheme).map_err(|_| "Invalid notification URL")?;
            socket::listen(url.as_str(), session.token(), |bytes| notify(bytes, &discovery, policy.browsing(), &remote_changes)).await
        };
        tokio::select! {
            biased;
            result = activity.changed() => { if result.is_err() { return; } retry = 1; continue; },
            result = changed.changed() => { if result.is_err() { return; } retry = 1; continue; },
            result = attempt => { if let Err(error) = result { log::debug!("Notification connection ended: {error}"); } },
        }
        tokio::select! {
            result = activity.changed() => { if result.is_err() { return; } retry = 1; continue; },
            result = changed.changed() => { if result.is_err() { return; } retry = 1; },
            _ = crate::executor::sleep(Duration::from_secs(retry)) => { retry = (retry * 2).min(30); },
        }
    }
}

fn notify(bytes: &[u8], discovery: &SyncScheduler, browsing: bool, remote_changes: &crate::notification_interest::RemoteChanges) -> Result<(), String> {
    let message = sync_common::transport::decode::<NotificationMessage>(bytes, 4096).map_err(|e| e.to_string())?;
    // Hello reconciles after initial connection or reconnection.
    match message {
        NotificationMessage::Hello { .. } if !browsing => {}
        NotificationMessage::LibraryChanged { .. } => remote_changes.request(),
        NotificationMessage::LibrariesChanged => {
            remote_changes.request();
            discovery.request_remote_refresh();
        }
        NotificationMessage::Hello { .. } | NotificationMessage::Reconcile => {
            remote_changes.request();
            discovery.request_remote_refresh();
        }
    }
    Ok(())
}

#[cfg(all(target_arch = "wasm32", feature = "web-runtime-tests"))]
pub(super) async fn socket_contract(url: &str, token: &str) -> Result<u32, String> {
    let (scheduler, requests) = SyncScheduler::new();
    let (discovery, discoveries) = SyncScheduler::new();
    socket::listen(url, token, |bytes| notify(bytes, &discovery, true, &crate::notification_interest::RemoteChanges::default())).await?;
    drop(discoveries);
    Ok(std::iter::from_fn(|| requests.try_recv().ok()).count() as u32)
}

fn receive(bytes: &[u8], timeout: &mut Duration, received: &mut impl FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
    if let Ok(NotificationMessage::Hello { timeout_seconds, .. }) = sync_common::transport::decode(bytes, 4096) {
        *timeout = Duration::from_secs(timeout_seconds.max(1));
    }
    received(bytes)
}

#[cfg(not(target_arch = "wasm32"))]
#[path = "notifications/native.rs"]
mod socket;
#[cfg(target_arch = "wasm32")]
#[path = "notifications/web.rs"]
mod socket;

impl super::core::AppBackend {
    pub(crate) fn notification_interest(&self, browsing: bool) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        let interest = self.state.notification_interest.view(browsing);
        self.state.executor.spawn_detached(async move {
            let _interest = interest;
            sender.closed().await;
        });
        receiver
    }
}
