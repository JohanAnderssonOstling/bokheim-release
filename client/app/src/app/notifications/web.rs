use std::time::Duration;
pub(super) async fn listen(url: &str, token: &str, mut received: impl FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
    let bearer = format!("bearer.{token}");
    let connection = client_platform_web::transport::socket::Connection::new(url, &["bokheim.notifications.v1", &bearer])?;
    let mut timeout = Duration::from_secs(15);
    while let Ok(bytes) = crate::executor::timeout(timeout, connection.receive()).await.map_err(|_| "Notification heartbeat timed out")? {
        if bytes.is_empty() {
            connection.send(&[]).map_err(|_| "Notification heartbeat failed")?;
        } else {
            super::receive(&bytes, &mut timeout, &mut received)?;
        }
    }
    Ok(())
}
