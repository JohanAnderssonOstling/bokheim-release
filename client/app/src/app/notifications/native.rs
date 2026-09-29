use client_platform_native::socket::{Connection, Message};
use std::time::Duration;
pub(super) async fn listen(url: &str, token: &str, mut received: impl FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
    let bearer = format!("Bearer {token}");
    let headers = [("Authorization", bearer.as_str())];
    let mut socket = crate::executor::timeout(Duration::from_secs(15), Connection::connect(url, &headers)).await.map_err(|_| "Notification connection timed out")?.map_err(|_| "Notification connection failed")?;
    let mut timeout = Duration::from_secs(15);
    while let Some(message) = crate::executor::timeout(timeout, socket.receive()).await.map_err(|_| "Notification heartbeat timed out")? {
        match message.map_err(|_| "Notification connection failed")? {
            Message::Binary(bytes) if bytes.is_empty() => socket.send_binary(bytes).await.map_err(|_| "Notification heartbeat failed")?,
            Message::Binary(bytes) => super::receive(&bytes, &mut timeout, &mut received)?,
            Message::Ping(bytes) => socket.pong(bytes).await.map_err(|_| "Notification heartbeat failed")?,
            Message::Closed => break,
            _ => {}
        }
    }
    Ok(())
}
