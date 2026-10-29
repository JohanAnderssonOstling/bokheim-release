//! Byte fetches run on their owning executor; readers own cancellable replies.

use crate::{FetchFuture, RemoteFileError};
use futures_channel::oneshot;
use futures_util::future::{select, BoxFuture, Either};
use std::{io, sync::Arc};

pub(crate) type Fetch<Args> = Arc<dyn Fn(Args) -> BoxFuture<'static, io::Result<Vec<u8>>> + Send + Sync>;

async fn send_response<E>(response: FetchFuture<'static, Result<Vec<u8>, E>>, mut reply: oneshot::Sender<io::Result<Vec<u8>>>)
where
    E: Into<RemoteFileError> + 'static,
{
    if reply.is_canceled() {
        return;
    }
    let response = Box::pin(crate::time::timeout(crate::remote_file::REQUEST_TIMEOUT, response));
    let result = match select(response, reply.cancellation()).await {
        Either::Left((result, _)) => result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "remote file read timed out")).and_then(|result| result.map_err(|error| io::Error::other(error.into()))),
        Either::Right(_) => return,
    };
    let _ = reply.send(result);
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn native<Args: Send + 'static>(fetch: impl Fn(Args) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + Sync + 'static) -> io::Result<Fetch<Args>> {
    let handle = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
    let fetch = Arc::new(fetch);
    Ok(Arc::new(move |args| {
        let fetch = fetch.clone();
        let (reply, response) = oneshot::channel();
        handle.spawn(async move { send_response(fetch(args), reply).await });
        Box::pin(async move { response.await.map_err(|_| io::Error::other("book range request canceled"))? })
    }))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn browser<Args: Send + 'static>(mut fetch: impl FnMut(Args) -> FetchFuture<'static, Result<Vec<u8>, String>> + 'static) -> Fetch<Args> {
    let (sender, receiver) = async_channel::unbounded::<(Args, oneshot::Sender<io::Result<Vec<u8>>>)>();
    wasm_bindgen_futures::spawn_local(async move {
        while let Ok((args, reply)) = receiver.recv().await {
            if !reply.is_canceled() {
                let response = fetch(args);
                wasm_bindgen_futures::spawn_local(send_response(response, reply));
            }
        }
    });
    Arc::new(move |args| {
        let (reply, response) = oneshot::channel();
        let sent = sender.try_send((args, reply));
        Box::pin(async move {
            sent.map_err(|_| io::Error::other("book source service stopped"))?;
            response.await.map_err(|_| io::Error::other("book range request canceled"))?
        })
    })
}
