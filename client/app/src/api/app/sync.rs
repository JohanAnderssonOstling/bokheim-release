use super::AppClient;
use crate::runtime::AppCommand;

impl AppClient {
    /// Hold while a foreground view is alive. Dropping the receiver releases
    /// its interest; opening it never waits for a network connection or refresh.
    pub async fn notification_interest(&self, browsing: bool) -> Result<async_channel::Receiver<()>, String> {
        self.transport.notification_interest_transport(browsing).await.map_err(|error| error.to_string())
    }

    pub async fn transfers(&self) -> Result<Vec<crate::LibraryTransfers>, String> {
        self.request(AppCommand::Transfers).await
    }
}
