//! Connection transitions without a polling timer. Hosts without an availability
//! service retain their existing unrestricted network behavior.
use std::future::pending;
use tokio::sync::watch;

pub struct Connectivity {
    online: bool,
    changes: Option<watch::Receiver<bool>>,
}

impl Connectivity {
    pub fn unrestricted() -> Self {
        Self { online: true, changes: None }
    }

    pub fn new(mut changes: watch::Receiver<bool>) -> Self {
        let online = *changes.borrow_and_update();
        Self { online, changes: Some(changes) }
    }

    pub fn online(&self) -> bool {
        self.online
    }

    pub async fn wait_online(&mut self) {
        while !self.online {
            self.changed().await;
        }
    }

    /// Also catches a transition published before this future's first poll.
    pub async fn changed(&mut self) {
        let Some(changes) = &mut self.changes else {
            return pending().await;
        };
        loop {
            let online = *changes.borrow_and_update();
            if online != self.online {
                self.online = online;
                return;
            }
            if changes.changed().await.is_err() {
                return pending().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::poll;

    #[tokio::test]
    async fn transitions_cannot_be_lost_and_unchanged_observations_do_not_wake_work() {
        let (sender, receiver) = watch::channel(false);
        let mut network = Connectivity::new(receiver);
        assert!(!network.online());
        sender.send_replace(true);
        network.changed().await;
        assert!(network.online());
        let mut waiting = Box::pin(network.changed());
        assert!(poll!(&mut waiting).is_pending());
        sender.send_replace(true);
        assert!(poll!(&mut waiting).is_pending());
        sender.send_replace(false);
        waiting.await;
        assert!(!network.online());
        let mut unrestricted = Connectivity::unrestricted();
        assert!(poll!(Box::pin(unrestricted.changed())).is_pending());
    }
}
