use crate::{
    runtime::{AppCommand, LibraryReply, Transport},
    LibraryId,
};
use library_backend::LibraryClient;

/// The frontend entry point for running backend operations.
/// Backend implementation objects are deliberately not part of this API.
///
/// ```compile_fail
/// use app::app::{AppBackend, BackendContext};
/// ```
/// ```compile_fail
/// use app::library::LibrarySession;
/// ```
#[derive(Clone)]
pub struct AppClient {
    pub(crate) transport: Transport,
}

impl AppClient {
    pub(crate) async fn request<T: LibraryReply>(&self, command: AppCommand) -> Result<T, String> {
        self.transport.request(command).await.map_err(|error| error.to_string())
    }

    pub fn library(&self, library_id: LibraryId) -> LibraryClient {
        crate::library_client::connect(self.clone(), library_id)
    }
}
