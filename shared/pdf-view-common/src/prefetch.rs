/// One prefetch window's cancellation, shared by the UI, parser and I/O worker.
/// Closing the channel wakes async I/O and a browser parser's blocking bridge.
#[derive(Clone)]
pub struct PdfPrefetchCancellation {
    sender: async_channel::Sender<()>,
    receiver: async_channel::Receiver<()>,
}

impl Default for PdfPrefetchCancellation {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(1);
        Self { sender, receiver }
    }
}

impl PdfPrefetchCancellation {
    pub fn cancel(&self) {
        self.sender.close();
    }

    pub fn is_cancelled(&self) -> bool {
        self.receiver.is_closed()
    }

    pub async fn cancelled(&self) {
        let _ = self.receiver.recv().await;
    }
}
