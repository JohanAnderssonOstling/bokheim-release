//! Native WebSocket ownership and TLS. Callers supply credentials and timing policy.
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message as Frame},
    MaybeTlsStream, WebSocketStream,
};

pub enum Message {
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Closed,
    Other,
}
/// Dropping the connection releases its TCP/TLS stream, including cancellation.
pub struct Connection(WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>);
fn request(url: &str, headers: &[(&str, &str)]) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, String> {
    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    for (name, value) in headers {
        let name = name.parse::<tokio_tungstenite::tungstenite::http::HeaderName>().map_err(|_| "Invalid socket header")?;
        request.headers_mut().insert(name, value.parse().map_err(|_| "Invalid socket credential")?);
    }
    Ok(request)
}
impl Connection {
    pub async fn connect(url: &str, headers: &[(&str, &str)]) -> Result<Self, String> {
        let request = request(url, headers)?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio_tungstenite::connect_async(request).await.map(|(socket, _)| Self(socket)).map_err(|e| e.to_string())
    }
    pub async fn receive(&mut self) -> Option<Result<Message, String>> {
        self.0.next().await.map(|frame| {
            frame
                .map(|frame| match frame {
                    Frame::Binary(bytes) => Message::Binary(bytes.to_vec()),
                    Frame::Ping(bytes) => Message::Ping(bytes.to_vec()),
                    Frame::Close(_) => Message::Closed,
                    _ => Message::Other,
                })
                .map_err(|e| e.to_string())
        })
    }
    pub async fn send_binary(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.0.send(Frame::Binary(bytes.into())).await.map_err(|e| e.to_string())
    }
    pub async fn pong(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.0.send(Frame::Pong(bytes.into())).await.map_err(|e| e.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_stay_in_headers_and_cannot_inject_headers() {
        let request = request("wss://example.test/events", &[("Authorization", "Bearer fixture")]).unwrap();
        assert_eq!(request.uri().to_string(), "wss://example.test/events");
        assert_eq!(request.headers()["authorization"], "Bearer fixture");
        assert!(super::request("wss://example.test/events", &[("Authorization", "Bearer bad\r\nother: injected")]).is_err());
    }
}
