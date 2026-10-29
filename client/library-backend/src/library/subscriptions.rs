//! Subscription requests and live resources owned by one library.

use super::handle::LibraryActorCommand;
use async_channel::{Receiver, RecvError, Sender, TryRecvError};
use library_model::LibraryUpdate;

/// Receives updates from one actor generation. Retirement closes the command
/// channel, so a retained receiver stops even while that actor is unpolled.
#[derive(Clone)]
pub struct LibraryUpdates {
    events: Receiver<LibraryUpdate>,
    commands: Option<Sender<LibraryActorCommand>>,
}

impl LibraryUpdates {
    pub(crate) fn new(events: Receiver<LibraryUpdate>, commands: Sender<LibraryActorCommand>) -> Self {
        Self { events, commands: Some(commands) }
    }

    pub async fn recv(&self) -> Result<LibraryUpdate, RecvError> {
        match &self.commands {
            Some(commands) => tokio::select! {
                biased;
                _ = commands.closed() => Err(RecvError),
                update = self.events.recv() => update,
            },
            None => self.events.recv().await,
        }
    }

    pub fn try_recv(&self) -> Result<LibraryUpdate, TryRecvError> {
        if self.commands.as_ref().is_some_and(|commands| commands.is_closed()) {
            return Err(TryRecvError::Closed);
        }
        self.events.try_recv()
    }
}

impl From<Receiver<LibraryUpdate>> for LibraryUpdates {
    fn from(events: Receiver<LibraryUpdate>) -> Self {
        Self { events, commands: None }
    }
}

#[cfg(target_arch = "wasm32")]
use crate::BookId;
use client_runtime::reply::Reply;
use client_runtime::wire::{encode_reply, WorkerPayload};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum LibrarySubscription {
    Updates,
    #[cfg(target_arch = "wasm32")]
    Book {
        content_hash: BookId,
    },
}

pub enum LibraryEvents {
    #[cfg(target_arch = "wasm32")]
    Book(Box<dyn crate::executor::BackendSend>),
    Updates(LibraryUpdates),
}
impl LibraryEvents {
    pub async fn next_value(&self) -> Option<WorkerPayload> {
        match self {
            #[cfg(target_arch = "wasm32")]
            Self::Book(_lease) => std::future::pending().await,
            Self::Updates(receiver) => receiver.recv().await.ok().and_then(|update| encode_reply(update).ok()),
        }
    }
}

impl super::LibraryHandle {
    pub async fn subscribe(&self, subscription: LibrarySubscription) -> Result<(Reply, LibraryEvents), crate::BackendError> {
        match subscription {
            LibrarySubscription::Updates => {
                let (events, scanning) = self.subscribe_updates().await?;
                Ok((Reply::new(scanning), LibraryEvents::Updates(events)))
            }
            #[cfg(target_arch = "wasm32")]
            LibrarySubscription::Book { content_hash } => {
                let (initial, lease) = self.subscribe_book(content_hash).await?;
                Ok((initial, LibraryEvents::Book(Box::new(lease))))
            }
        }
    }
}
