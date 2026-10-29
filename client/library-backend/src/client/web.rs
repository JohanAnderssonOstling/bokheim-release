use crate::library::LibraryCommand;
use crate::BookId;
use crate::ResolvedBookData;

impl crate::library::web_reader::BookTransport for super::LibraryClient {
    async fn request<T: crate::library::commands::LibraryReply>(&self, command: LibraryCommand) -> Result<T, crate::BackendError> {
        self.request(command).await
    }

    async fn subscribe_book(&self, content_hash: BookId) -> Result<(async_channel::Receiver<()>, ResolvedBookData), crate::BackendError> {
        self.transport.subscribe_book(content_hash).await
    }
}
