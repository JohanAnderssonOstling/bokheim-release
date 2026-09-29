use wasm_bindgen::{prelude::*, JsCast};
pub struct Connection {
    messages: async_channel::Receiver<Vec<u8>>,
    socket: web_sys::WebSocket,
    _message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _closed: Closure<dyn FnMut(web_sys::Event)>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        let _ = self.socket.close();
    }
}
impl Connection {
    pub fn new(url: &str, protocols: &[&str]) -> Result<Self, String> {
        let protocols = protocols.iter().map(|p| JsValue::from_str(p)).collect::<js_sys::Array>();
        let socket = web_sys::WebSocket::new_with_str_sequence(url, &protocols).map_err(|_| "Notification connection failed")?;
        socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let (send, messages) = async_channel::unbounded();
        let closed = send.clone();
        let message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                let _ = send.try_send(js_sys::Uint8Array::new(&buffer).to_vec());
            } else {
                send.close();
            }
        });
        let closed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            closed.close();
        });
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(closed.as_ref().unchecked_ref()));
        socket.set_onerror(Some(closed.as_ref().unchecked_ref()));
        Ok(Self { socket, messages, _message: message, _closed: closed })
    }
    pub async fn receive(&self) -> Result<Vec<u8>, async_channel::RecvError> {
        self.messages.recv().await
    }
    pub fn send(&self, bytes: &[u8]) -> Result<(), String> {
        self.socket.send_with_u8_array(bytes).map_err(|_| "WebSocket send failed".into())
    }
}
