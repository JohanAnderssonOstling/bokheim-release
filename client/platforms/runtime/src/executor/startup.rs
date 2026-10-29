//! Background side effects wait for the host to commit the startup generation.

use std::future::pending;
use std::sync::{Mutex, PoisonError};
use tokio::sync::watch;

// Each browser database worker has its own WASM instance and gate. Native
// executors share the process gate across their runtime and library threads.
static STARTUP_GATE: Mutex<Option<watch::Receiver<bool>>> = Mutex::new(None);

pub struct StartupPermit(watch::Sender<bool>);
impl StartupPermit {
    pub fn release(self) {
        *STARTUP_GATE.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.0.send_replace(true);
    }
}

pub fn defer_background_work() -> StartupPermit {
    let (sender, receiver) = watch::channel(false);
    let mut gate = STARTUP_GATE.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(gate.is_none(), "startup work is already deferred");
    *gate = Some(receiver);
    StartupPermit(sender)
}

pub(super) async fn startup_ready() {
    let receiver = STARTUP_GATE.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(receiver) = receiver {
        wait_for_commit(receiver).await;
    }
}

async fn wait_for_commit(mut receiver: watch::Receiver<bool>) {
    if receiver.wait_for(|ready| *ready).await.is_err() {
        // Dropping a trial's permit never authorizes background side effects.
        pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_commit_releases_current_and_late_waiters() {
        let (permit, receiver) = watch::channel(false);
        let mut waiting = Box::pin(wait_for_commit(receiver.clone()));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        permit.send_replace(true);
        drop(permit);
        waiting.await;
        // The committed value remains valid after the permit is consumed.
        wait_for_commit(receiver).await;
    }

    #[tokio::test]
    async fn abandoned_startup_keeps_current_and_late_waiters_blocked() {
        let (permit, receiver) = watch::channel(false);
        let mut waiting = Box::pin(wait_for_commit(receiver.clone()));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(permit);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        let mut late = Box::pin(wait_for_commit(receiver));
        assert!(futures_util::poll!(&mut late).is_pending());
    }
}
