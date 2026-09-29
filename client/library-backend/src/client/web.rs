#[cfg(feature = "web-runtime-tests")]
use crate::BookSource;
use crate::ResolvedBookData;

impl crate::library::web_reader::BookTransport for super::LibraryClient {
    async fn request<T: crate::library::commands::LibraryReply>(&self, command: crate::library::LibraryCommand) -> Result<T, String> {
        self.request(command).await
    }

    async fn subscribe_book(&self, content_hash: crate::ContentHash) -> Result<(async_channel::Receiver<()>, ResolvedBookData), String> {
        self.transport.subscribe_book(content_hash).await
    }
}

#[cfg(feature = "web-runtime-tests")]
impl super::LibraryClient {
    /// A page cannot perform blocking parser reads. Browser probes exercise the
    /// same subscription and range commands asynchronously instead.
    pub async fn book_prefix(&self, content_hash: crate::ContentHash, length: usize) -> Result<Vec<u8>, String> {
        let (_lease, book) = self.transport.subscribe_book(content_hash).await?;
        let BookSource::Local { capability, .. } = book.source else {
            return Err("expected a local document".into());
        };
        let bytes: serde_bytes::ByteBuf = self.request(crate::library::commands::library_requests::ReadBookRange { capability, offset: 0, length }).await?;
        Ok(bytes.into_vec())
    }
}

impl super::LibraryClient {
    pub fn resolve_book_diagnostics(&self) -> async_channel::Receiver<String> {
        self.transport.diagnostics()
    }
}
