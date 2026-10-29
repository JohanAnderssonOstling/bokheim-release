//! Account-scoped wakeups. Reconnect reconciles missed events; no remote polling.

use crate::DeviceAccountSession;
use client_platform_runtime::connectivity::Connectivity;
use library_backend::library::{LibraryDirectory, LibraryNotifications};
use std::sync::Arc;
use std::time::Duration;
use sync_common::api::notifications::NotificationMessage;
use tokio::sync::watch;

pub(super) async fn run(account: DeviceAccountSession, account_wake: watch::Sender<()>, interest: crate::notification_interest::NotificationInterest, libraries: Arc<LibraryDirectory>) {
    let network = match super::network::observe() {
        Ok(network) => network,
        Err(error) => { log::warn!("Could not observe notification connectivity: {error}"); return; }
    };
    run_with_network(account, account_wake, interest, libraries, network).await;
}

async fn run_with_network(account: DeviceAccountSession, account_wake: watch::Sender<()>, interest: crate::notification_interest::NotificationInterest, libraries: Arc<LibraryDirectory>, mut network: Connectivity) {
    let mut activity = interest.subscribe();
    let initial_activity = *activity.borrow_and_update();
    let mut library_notifications = LibraryNotifications::new(libraries, account.library_account(), initial_activity);
    // Session changes close the old connection and reconnect with the latest
    // credentials. Account maintenance observes session changes separately.
    let mut changed = account.subscribe();
    changed.borrow_and_update();
    let mut retry = 1;
    loop {
        let policy = *activity.borrow_and_update();
        if library_notifications.observe_activity(policy) {
            account_wake.send_replace(());
        }
        let online = network.online();
        let attempt = async {
            if !policy.enabled() || !online {
                return std::future::pending().await;
            }
            let session = account.refresh_if_needed().await.map_err(|e| e.to_string())?;
            let Some(session) = session else { return std::future::pending().await };
            let mut url = session.server_url().join("api/notifications").map_err(|error| error.to_string())?;
            let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
            url.set_scheme(scheme).map_err(|_| "Invalid notification URL")?;
            socket::listen(url.as_str(), session.token(), |bytes| notify(bytes, &account_wake, policy.browsing(), &library_notifications)).await
        };
        tokio::select! {
            biased;
            _ = network.changed() => { retry = 1; continue; },
            result = activity.changed() => { if result.is_err() { return; } retry = 1; continue; },
            result = changed.changed() => { if result.is_err() { return; } retry = 1; continue; },
            result = attempt => { if let Err(error) = result { log::debug!("Notification connection ended: {error}"); } },
        }
        tokio::select! {
            _ = network.changed() => { retry = 1; continue; },
            result = activity.changed() => { if result.is_err() { return; } retry = 1; continue; },
            result = changed.changed() => { if result.is_err() { return; } retry = 1; },
            result = crate::executor::sleep(Duration::from_secs(retry)) => {
                if let Err(error) = result {
                    log::warn!("Notification reconnects stopped: {error}");
                    return;
                }
                retry = (retry * 2).min(30);
            },
        }
    }
}

fn notify(bytes: &[u8], account_wake: &watch::Sender<()>, browsing: bool, libraries: &LibraryNotifications) -> Result<(), String> {
    let message = sync_common::transport::decode::<NotificationMessage>(bytes, 4096).map_err(|e| e.to_string())?;
    libraries.receive(&message, browsing).map_err(|error| error.to_string())?;
    if matches!(message, NotificationMessage::LibrariesChanged | NotificationMessage::Reconcile) || (browsing && matches!(message, NotificationMessage::Hello { .. })) {
        account_wake.send_replace(());
    }
    Ok(())
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::notification_interest::InterestState;
    use crate::{AppDataLocation, BackendContext};

    #[tokio::test(start_paused = true)]
    async fn subscribed_notifications_have_no_retry_timer_while_offline() {
        use crate::notification_interest::NotificationInterest;
        use futures_util::task::{waker_ref, ArcWake};
        use std::future::Future;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::Context;
        struct Wakes(AtomicUsize);
        impl ArcWake for Wakes {
            fn wake_by_ref(waker: &Arc<Self>) { waker.0.fetch_add(1, Ordering::Relaxed); }
        }
        let root = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap();
        let server = "http://127.0.0.1:1".parse().unwrap();
        let account = DeviceAccountSession::load(context, server).unwrap();
        account.set(account_client::Session::try_new("http://127.0.0.1:1".parse().unwrap(), "user".into(), "reader@example.com".into(), "a".repeat(43)).unwrap()).unwrap();
        let interest = NotificationInterest::default();
        let _browsing = interest.view(true);
        let (wake, _requests) = watch::channel(());
        let (connection, status) = watch::channel(false);
        let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
        let waker = waker_ref(&wakes);
        let mut cx = Context::from_waker(&waker);
        let mut notifications = Box::pin(run_with_network(account, wake, interest, Arc::new(LibraryDirectory::default()), Connectivity::new(status)));
        assert!(notifications.as_mut().poll(&mut cx).is_pending());
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(wakes.0.load(Ordering::Relaxed), 0);
        connection.send_replace(true);
        assert!(wakes.0.load(Ordering::Relaxed) > 0, "reconnect schedules an immediate notification connection");
    }

    #[test]
    fn account_discovery_wakes_for_account_events_without_open_libraries() {
        let root = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap();
        let account = DeviceAccountSession::load(context, "https://example.com".parse().unwrap()).unwrap();
        let libraries = LibraryNotifications::new(Arc::new(LibraryDirectory::default()), account.library_account(), InterestState::default());
        let (wake, mut changes) = watch::channel(());
        for (message, browsing, should_wake) in [
            (NotificationMessage::LibraryChanged { library_id: sync_common::LibraryId::new_v4() }, true, false),
            (NotificationMessage::Hello { heartbeat_seconds: 5, timeout_seconds: 15 }, false, false),
            (NotificationMessage::Hello { heartbeat_seconds: 5, timeout_seconds: 15 }, true, true),
            (NotificationMessage::LibrariesChanged, false, true),
            (NotificationMessage::Reconcile, false, true),
        ] {
            let bytes = sync_common::transport::encode(&message).unwrap();
            notify(&bytes, &wake, browsing, &libraries).unwrap();
            assert_eq!(changes.has_changed().unwrap(), should_wake, "{message:?}");
            changes.borrow_and_update();
        }
    }
}
