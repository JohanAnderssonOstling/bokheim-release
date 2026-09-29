//! Subscription requests and live resources owned by one library.
use client_runtime::reply::Reply;
use client_runtime::wire::{encode_reply, WorkerPayload};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum LibrarySubscription {
    Updates,
    BookDownload {
        content_hash: crate::ContentHash,
    },
    #[cfg(target_arch = "wasm32")]
    Book {
        content_hash: crate::ContentHash,
    },
}
impl LibrarySubscription {
    /// Reconstruct refresh events from the initial reply after a worker reconnect.
    pub fn restored_updates(&self, initial: WorkerPayload) -> Result<Vec<WorkerPayload>, crate::BackendError> {
        match self {
            Self::Updates => {
                let scanning = client_runtime::wire::decode_worker_payload::<bool>(initial)?;
                [library_model::LibraryUpdate::Contents, library_model::LibraryUpdate::Scanning(scanning)].into_iter().map(encode_reply).collect()
            }
            _ => Ok(vec![initial]),
        }
    }

    /// Book sources hold generation-specific leases and cannot be replayed after reconnect.
    pub fn is_book(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        if matches!(self, Self::Book { .. }) {
            return true;
        }
        false
    }
}

pub enum LibraryEvents {
    #[cfg(target_arch = "wasm32")]
    Book(Box<dyn crate::executor::BackendSend>),
    Updates(async_channel::Receiver<library_model::LibraryUpdate>),
    DownloadStates(async_channel::Receiver<crate::DownloadState>),
}
impl LibraryEvents {
    pub async fn next_value(&self) -> Option<WorkerPayload> {
        match self {
            #[cfg(target_arch = "wasm32")]
            Self::Book(_lease) => std::future::pending().await,
            Self::Updates(receiver) => receiver.recv().await.ok().and_then(|update| encode_reply(update).ok()),
            Self::DownloadStates(receiver) => receiver.recv().await.ok().and_then(|state| encode_reply(state).ok()),
        }
    }
}

impl super::LibraryHandle {
    pub async fn subscribe(&self, subscription: LibrarySubscription) -> Result<(Reply, LibraryEvents), crate::BackendError> {
        match subscription {
            LibrarySubscription::Updates => {
                let events = self.subscribe_updates();
                let scanning = self.scanning().await?;
                Ok((Reply::new(scanning), LibraryEvents::Updates(events)))
            }
            LibrarySubscription::BookDownload { content_hash } => {
                let (events, initial) = self.download_changes(content_hash).await?;
                Ok((Reply::new(initial), LibraryEvents::DownloadStates(events)))
            }
            #[cfg(target_arch = "wasm32")]
            LibrarySubscription::Book { content_hash } => {
                let (initial, lease) = self.subscribe_book(content_hash).await?;
                Ok((initial, LibraryEvents::Book(Box::new(lease))))
            }
        }
    }
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;
    use client_runtime::wire::decode_worker_payload;

    #[test]
    fn updates_refresh_contents_and_scanning_after_reconnect() {
        for scanning in [false, true] {
            let mut updates = LibrarySubscription::Updates.restored_updates(encode_reply(scanning).unwrap()).unwrap().into_iter();
            assert!(matches!(decode_worker_payload::<library_model::LibraryUpdate>(updates.next().unwrap()).unwrap(), library_model::LibraryUpdate::Contents));
            assert!(matches!(decode_worker_payload::<library_model::LibraryUpdate>(updates.next().unwrap()).unwrap(), library_model::LibraryUpdate::Scanning(value) if value == scanning));
            assert!(updates.next().is_none());
        }
    }

    #[test]
    fn download_reconnect_preserves_the_initial_payload() {
        let subscription = LibrarySubscription::BookDownload { content_hash: crate::ContentHash::new(&"b".repeat(64)) };
        let initial = encode_reply("current download state").unwrap();
        let updates = subscription.restored_updates(initial.clone()).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, initial.0);
    }

    #[test]
    fn updates_reject_invalid_initial_reply() {
        assert!(LibrarySubscription::Updates.restored_updates(encode_reply("invalid").unwrap()).is_err());
    }
}
