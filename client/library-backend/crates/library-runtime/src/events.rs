//! Event delivery owned by one concrete library session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use library_model::{BookDownloadStatus, DownloadState, LibraryUpdate};
use sync_common::ContentHash;

#[derive(Clone, Debug)]
pub enum LibraryEvent {
    ThumbnailGenerated { content_hash: ContentHash },
    DownloadStatusChanged { content_hash: ContentHash, state: DownloadState },
    TransferStatusChanged,
    StorageChanged,
    ContentsChanged,
    ImportStarted,
    ImportComplete,
    ScanStarted,
    ScanComplete,
}

pub type LibraryEventSender = async_channel::Sender<LibraryEvent>;
pub type LibraryEventReceiver = async_channel::Receiver<LibraryEvent>;

pub fn library_event_channel() -> (LibraryEventSender, LibraryEventReceiver) {
    async_channel::unbounded()
}

/// UI subscriptions live with the session, never in global application state.
#[derive(Clone, Default)]
pub struct LibraryEventSubscriptions {
    subscribers: Arc<Mutex<LibraryEventSubscribers>>,
}

#[derive(Default)]
struct LibraryEventSubscribers {
    closed: bool,
    updates: Vec<async_channel::Sender<LibraryUpdate>>,
    downloads: Vec<(ContentHash, async_channel::Sender<DownloadState>)>,
    latest_downloads: HashMap<ContentHash, DownloadState>,
}

impl LibraryEventSubscriptions {
    /// Retirement is permanent, including subscriptions made through stale handles.
    pub fn close(&self) {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        subscribers.closed = true;
        for sender in subscribers.updates.drain(..) {
            sender.close();
        }
        for (_, sender) in subscribers.downloads.drain(..) {
            sender.close();
        }
    }

    pub fn subscribe_updates(&self) -> async_channel::Receiver<LibraryUpdate> {
        let (sender, receiver) = async_channel::unbounded();
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !subscribers.closed {
            subscribers.updates.push(sender);
        }
        receiver
    }

    pub fn subscribe_download(&self, content_hash: ContentHash) -> async_channel::Receiver<DownloadState> {
        let (sender, receiver) = async_channel::unbounded();
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if subscribers.closed {
            return receiver;
        }
        subscribers.downloads.retain(|(_, sender)| !sender.is_closed());
        subscribers.downloads.push((content_hash, sender));
        receiver
    }

    /// The durable baseline is read before this lock. An event published in
    /// between replaces it; an event published after registration goes to the
    /// receiver. Thus the initial value cannot be newer than a queued event.
    pub fn subscribe_download_with_initial(&self, content_hash: ContentHash, durable: DownloadState) -> (async_channel::Receiver<DownloadState>, DownloadState) {
        let (sender, receiver) = async_channel::unbounded();
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let initial = subscribers.latest_downloads.get(&content_hash).cloned().unwrap_or(durable);
        if !subscribers.closed {
            subscribers.downloads.retain(|(_, sender)| !sender.is_closed());
            subscribers.downloads.push((content_hash, sender));
        }
        (receiver, initial)
    }

    pub fn current_download(&self, content_hash: &ContentHash) -> Option<DownloadState> {
        self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).latest_downloads.get(content_hash).cloned()
    }

    /// A new explicit request after local eviction starts a new lifecycle.
    pub fn forget_terminal_download(&self, content_hash: &ContentHash) {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(subscribers.latest_downloads.get(content_hash), Some(DownloadState::Downloaded)) {
            subscribers.latest_downloads.remove(content_hash);
        }
    }

    pub fn publish(&self, event: LibraryEvent, busy: bool) {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let update = match event {
            LibraryEvent::ThumbnailGenerated { content_hash } => LibraryUpdate::Covers(vec![content_hash]),
            LibraryEvent::DownloadStatusChanged { content_hash, state } => {
                if subscribers.latest_downloads.get(&content_hash) == Some(&state) {
                    return;
                }
                if subscribers.latest_downloads.get(&content_hash).is_some_and(|previous| matches!((previous, &state), (DownloadState::Downloaded, DownloadState::Queued | DownloadState::Downloading(_)))) {
                    return;
                }
                subscribers.latest_downloads.insert(content_hash, state.clone());
                subscribers.downloads.retain(|(subscribed_hash, sender)| *subscribed_hash != content_hash || sender.try_send(state.clone()).is_ok());
                LibraryUpdate::Download(BookDownloadStatus::new(content_hash, state))
            }
            LibraryEvent::TransferStatusChanged | LibraryEvent::StorageChanged => return,
            LibraryEvent::ScanStarted | LibraryEvent::ImportStarted => LibraryUpdate::Scanning(busy),
            LibraryEvent::ContentsChanged => LibraryUpdate::Contents,
            LibraryEvent::ScanComplete | LibraryEvent::ImportComplete => {
                send_update(&mut subscribers.updates, LibraryUpdate::Scanning(busy));
                LibraryUpdate::Contents
            }
        };
        send_update(&mut subscribers.updates, update);
    }

    pub fn publish_update(&self, update: LibraryUpdate) {
        send_update(&mut self.subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).updates, update);
    }
}

fn send_update(subscribers: &mut Vec<async_channel::Sender<LibraryUpdate>>, update: LibraryUpdate) {
    subscribers.retain(|sender| sender.try_send(update.clone()).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retirement_closes_existing_and_future_subscriptions() {
        let subscriptions = LibraryEventSubscriptions::default();
        let retained = subscriptions.clone();
        let hash = ContentHash::new(&format!("{:064x}", 1));
        let updates = retained.subscribe_updates();
        let downloads = retained.subscribe_download(hash);
        subscriptions.publish_update(LibraryUpdate::Contents);

        subscriptions.close();
        subscriptions.close(); // Repeated shutdown and actor drop are harmless.
        assert!(updates.is_closed());
        assert!(downloads.is_closed());
        assert!(matches!(updates.try_recv(), Ok(LibraryUpdate::Contents)));
        assert!(matches!(updates.try_recv(), Err(async_channel::TryRecvError::Closed)));
        assert!(matches!(downloads.try_recv(), Err(async_channel::TryRecvError::Closed)));
        assert!(retained.subscribe_updates().is_closed());
        assert!(retained.subscribe_download(hash).is_closed());
    }

    #[test]
    fn current_download_is_shared_and_terminal_states_do_not_regress() {
        let subscriptions = LibraryEventSubscriptions::default();
        let hash = ContentHash::new(&format!("{:064x}", 7));
        let updates = subscriptions.subscribe_updates();
        let book = subscriptions.subscribe_download(hash);
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Queued }, false);
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Downloaded }, false);
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Queued }, false);
        assert_eq!(subscriptions.current_download(&hash), Some(DownloadState::Downloaded));
        assert_eq!(book.try_recv().unwrap(), DownloadState::Queued);
        assert_eq!(book.try_recv().unwrap(), DownloadState::Downloaded);
        assert!(book.try_recv().is_err());
        assert!(matches!(updates.try_recv(), Ok(LibraryUpdate::Download(_))));
        assert!(matches!(updates.try_recv(), Ok(LibraryUpdate::Download(_))));
        assert!(updates.try_recv().is_err());
    }

    #[test]
    fn retry_resets_progress_and_new_request_can_follow_eviction() {
        let subscriptions = LibraryEventSubscriptions::default();
        let hash = ContentHash::new(&format!("{:064x}", 8));
        let book = subscriptions.subscribe_download(hash);
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Downloading(library_model::DownloadProgress::ZERO) }, false);
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Queued }, false);
        assert_eq!(subscriptions.current_download(&hash), Some(DownloadState::Queued));
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Downloaded }, false);
        subscriptions.forget_terminal_download(&hash);
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Queued }, false);
        assert_eq!(subscriptions.current_download(&hash), Some(DownloadState::Queued));
        assert_eq!(book.try_recv().unwrap(), DownloadState::Downloading(library_model::DownloadProgress::ZERO));
        assert_eq!(book.try_recv().unwrap(), DownloadState::Queued);
        assert_eq!(book.try_recv().unwrap(), DownloadState::Downloaded);
        assert_eq!(book.try_recv().unwrap(), DownloadState::Queued);
    }

    #[test]
    fn subscription_snapshot_orders_events_across_initialization() {
        let subscriptions = LibraryEventSubscriptions::default();
        let hash = ContentHash::new(&format!("{:064x}", 9));
        let durable_before_event = DownloadState::NotDownloaded;
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Queued }, false);
        let (events, initial) = subscriptions.subscribe_download_with_initial(hash, durable_before_event);
        assert_eq!(initial, DownloadState::Queued);
        assert!(events.try_recv().is_err());
        subscriptions.publish(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Downloaded }, false);
        assert_eq!(events.try_recv().unwrap(), DownloadState::Downloaded);
    }
}
