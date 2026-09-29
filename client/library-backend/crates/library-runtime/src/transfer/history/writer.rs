//! One ordered writer with a bounded, latest-snapshot mailbox.
use super::MessagePackTransferHistory;
use crate::TransferStatus;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct State {
    submitted: u64,
    completed: u64,
    pending: Option<(u64, VecDeque<TransferStatus>)>,
    error: Option<String>,
    stopping: bool,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

pub(crate) struct HistoryWriter {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl HistoryWriter {
    pub fn new(history: MessagePackTransferHistory) -> std::io::Result<Self> {
        Self::spawn(move |entries| history.store(entries).map_err(|error| error.to_string()))
    }

    pub(crate) fn spawn(mut store: impl FnMut(&VecDeque<TransferStatus>) -> Result<(), String> + Send + 'static) -> std::io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = std::thread::Builder::new().name("transfer-history".into()).spawn(move || {
            loop {
                let (sequence, entries) = {
                    let mut state = worker.state.lock().unwrap_or_else(|error| error.into_inner());
                    while state.pending.is_none() && !state.stopping {
                        state = worker.wake.wait(state).unwrap_or_else(|error| error.into_inner());
                    }
                    let Some(snapshot) = state.pending.take() else { return };
                    snapshot
                };
                // Neither the queue lock nor the mailbox lock spans serialization or I/O.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store(&entries))).unwrap_or_else(|_| Err("transfer history writer panicked".into()));
                if let Err(error) = &result {
                    log::warn!("Could not persist transfer history: {error}");
                }
                let mut state = worker.state.lock().unwrap_or_else(|error| error.into_inner());
                state.completed = sequence;
                state.error = result.err();
                worker.wake.notify_all();
            }
        })?;
        Ok(Self { shared, thread: Some(thread) })
    }

    /// Called under the queue lock, so snapshots enter the mailbox in completion order.
    /// Pending snapshots can be replaced because each contains the full bounded history.
    pub fn submit(&self, entries: VecDeque<TransferStatus>) {
        let mut state = self.shared.state.lock().unwrap_or_else(|error| error.into_inner());
        state.submitted += 1;
        state.pending = Some((state.submitted, entries));
        self.shared.wake.notify_all();
    }

    /// Wait for this call's snapshot or a newer one. Never call with the queue lock held.
    pub fn flush(&self) -> std::io::Result<()> {
        let mut state = self.shared.state.lock().unwrap_or_else(|error| error.into_inner());
        let target = state.submitted;
        while state.completed < target {
            state = self.shared.wake.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        match &state.error {
            Some(error) => Err(std::io::Error::other(error.clone())),
            None => Ok(()),
        }
    }
}
impl Drop for HistoryWriter {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|error| error.into_inner());
            state.stopping = true;
            self.shared.wake.notify_all();
        }
        // The worker drains its final snapshot before exiting, independent of Tokio shutdown.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
