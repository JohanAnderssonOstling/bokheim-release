#[derive(Clone)]
pub(crate) struct Transport {
    remote: std::rc::Rc<WebWorkerClient>,
}

use super::*;
use client_platform_runtime::pending::Pending;

use client_platform_web::transport::session::{Session, SessionMessage, SessionSender};

struct WebPendingRequest<'a> {
    client: &'a WebWorkerClient,
    id: u64,
    subscription: bool,
}

impl Drop for WebPendingRequest<'_> {
    fn drop(&mut self) {
        self.client.pending.take(self.id);
        if self.subscription {
            self.client.subscriptions.borrow_mut().remove(&self.id);
            self.client.subscription_commands.borrow_mut().remove(&self.id);
            if let Ok(bytes) = encode_worker_message(&AppWorkerClientMessage::CancelSubscription { id: self.id }) {
                let _ = self.client.port.send(&bytes);
            }
        }
    }
}

pub(crate) struct WebWorkerClient {
    _session: Session,
    _connection: crate::web::WebConnection,
    port: SessionSender,
    failure: std::rc::Rc<std::cell::RefCell<Option<crate::BackendError>>>,
    recovering: std::rc::Rc<std::cell::Cell<bool>>,
    subscription_commands: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<u64, Vec<u8>>>>,
    pending: std::rc::Rc<Pending<oneshot::Sender<Result<WorkerPayload, crate::BackendError>>>>,
    subscriptions: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<u64, async_channel::Sender<WorkerPayload>>>>,
    diagnostics: std::rc::Rc<std::cell::RefCell<Vec<async_channel::Sender<String>>>>,
}

impl Transport {
    pub(crate) async fn connect_web_transport(config: crate::web::WebConfiguration) -> Result<(Self, AppStartupState), crate::BackendError> {
        let connection = crate::web::WebConnection::connect(config).await?;
        Self::finish_web_connection(connection).await
    }

    async fn finish_web_connection(connection: crate::web::WebConnection) -> Result<(Self, AppStartupState), crate::BackendError> {
        let port = SessionSender::new(connection.worker.clone(), connection.port());

        let pending = std::rc::Rc::new(Pending::<oneshot::Sender<Result<WorkerPayload, crate::BackendError>>>::default());
        let (startup_sender, startup_receiver) = oneshot::channel();
        let startup_sender = std::rc::Rc::new(std::cell::RefCell::new(Some(startup_sender)));
        let failure = std::rc::Rc::new(std::cell::RefCell::new(None::<crate::BackendError>));
        let recovering = std::rc::Rc::new(std::cell::Cell::new(false));
        let subscription_commands = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::<u64, Vec<u8>>::new()));
        let response_pending = pending.clone();
        let subscriptions = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::<u64, async_channel::Sender<WorkerPayload>>::new()));
        let response_subscriptions = subscriptions.clone();
        let diagnostics = std::rc::Rc::new(std::cell::RefCell::new(Vec::<async_channel::Sender<String>>::new()));
        let response_diagnostics = diagnostics.clone();
        let fail: std::rc::Rc<dyn Fn(crate::BackendError)> = {
            let failure = failure.clone();
            let startup = startup_sender.clone();
            let pending = pending.clone();
            let subscriptions = subscriptions.clone();
            let diagnostics = diagnostics.clone();
            let port = port.clone();
            std::rc::Rc::new(move |error| {
                if failure.borrow().is_some() {
                    return;
                }
                *failure.borrow_mut() = Some(error.clone());
                if let Some(sender) = startup.borrow_mut().take() {
                    let _ = sender.send(Err(error.clone()));
                }
                pending.fail_all(|sender| {
                    let _ = sender.send(Err(error.clone()));
                });
                subscriptions.borrow_mut().clear();
                diagnostics.borrow_mut().clear();
                port.close();
            })
        };
        let message_failure = fail.clone();
        let message_recovering = recovering.clone();
        let message_commands = subscription_commands.clone();
        let message_port = port.clone();
        let on_message = move |message: SessionMessage| {
            if matches!(&message, SessionMessage::Reconnecting) {
                message_recovering.set(true);
                // A capability pins one worker-owned generation; reopening it
                // silently would mix new bytes with the reader's old cache.
                response_subscriptions.borrow_mut().retain(|id, _| message_commands.borrow().contains_key(id));
                response_pending.fail_all(|sender| {
                    let _ = sender.send(Err("Database connection interrupted. A write may already have committed; check its result before retrying.".into()));
                });
                return;
            }
            let parsed = match message {
                SessionMessage::Bytes(bytes) => decode_worker_message::<AppWorkerStartupMessage>(&bytes),
                SessionMessage::Failed(error) => Ok(AppWorkerStartupMessage::Failed { error: error.into() }),
                SessionMessage::Reconnecting => unreachable!(),
            };
            match parsed {
                Ok(AppWorkerStartupMessage::Ready { startup }) => {
                    if message_recovering.replace(false) {
                        for bytes in message_commands.borrow().values() {
                            if let Err(error) = message_port.send(bytes) {
                                message_failure(crate::BackendError::operation(error).context("Could not restore database subscription"));
                                return;
                            }
                        }
                    }
                    if let Some(sender) = startup_sender.borrow_mut().take() {
                        let _ = sender.send(Ok(startup));
                    }
                }
                Ok(AppWorkerStartupMessage::Failed { error }) => message_failure(error),
                Ok(AppWorkerStartupMessage::Diagnostic { message }) => {
                    response_diagnostics.borrow_mut().retain(|subscriber| subscriber.try_send(message.clone()).is_ok());
                }
                Ok(AppWorkerStartupMessage::Response { id, result }) => {
                    let pending = response_pending.take(id);
                    if let Some(sender) = pending {
                        let _ = sender.send(result);
                    } else {
                        let sender = response_subscriptions.borrow().get(&id).cloned();
                        let command = message_commands.borrow().get(&id).cloned();
                        if let (Some(sender), Some(command)) = (sender, command) {
                            // Reconnecting subscriptions must refresh views that
                            // may have missed events while the host was unavailable.
                            match restored_subscription_updates(&command, result) {
                                Ok(updates) => {
                                    for update in updates {
                                        let _ = sender.try_send(update);
                                    }
                                }
                                Err(error) => message_failure(crate::BackendError::operation(error).context("Could not restore database subscription")),
                            }
                        }
                    }
                }
                Ok(AppWorkerStartupMessage::Event { id, payload }) => {
                    if let Some(sender) = response_subscriptions.borrow().get(&id) {
                        let _ = sender.try_send(payload);
                    }
                }
                Err(error) => message_failure(error.context("invalid backend response")),
            }
        };
        let session = Session::new(port.clone(), on_message, move |error| fail(error.into()));
        // Readiness includes migrations over the user's entire library. Wait
        // for their result; transport/worker failures still resolve this channel.
        let startup = match startup_receiver.await {
            Ok(Ok(startup)) => startup,
            Ok(Err(error)) => {
                return Err(error);
            }
            Err(_) => {
                return Err("backend worker stopped during startup".into());
            }
        };
        Ok((Self { remote: std::rc::Rc::new(WebWorkerClient { _connection: connection, port, failure, recovering, subscription_commands, pending, subscriptions, diagnostics, _session: session }) }, startup))
    }

    pub(crate) fn web_diagnostics(&self) -> async_channel::Receiver<String> {
        let (sender, receiver) = async_channel::unbounded();
        self.remote.diagnostics.borrow_mut().push(sender);
        receiver
    }

    fn send_web_request(&self, command: WorkerCommand, events: Option<async_channel::Sender<WorkerPayload>>) -> Result<(u64, oneshot::Receiver<Result<WorkerPayload, crate::BackendError>>), crate::BackendError> {
        if let Some(error) = self.remote.failure.borrow().as_ref() {
            return Err(error.clone());
        }
        if self.remote.recovering.get() {
            return Err("Database connection is being restored. Try again shortly.".into());
        }
        let id = self.remote.pending.next_id();
        let restore = !command.is_book_subscription();
        let bytes = encode_worker_message(&AppWorkerClientMessage::Request(AppWorkerRequest { id, command }))?;
        let (sender, receiver) = oneshot::channel();
        self.remote.pending.insert(id, sender);
        if let Some(events) = events {
            if restore {
                self.remote.subscription_commands.borrow_mut().insert(id, bytes.clone());
            }
            self.remote.subscriptions.borrow_mut().insert(id, events);
        }
        if let Err(error) = self.remote.port.send(&bytes) {
            self.remote.pending.take(id);
            self.remote.subscriptions.borrow_mut().remove(&id);
            self.remote.subscription_commands.borrow_mut().remove(&id);
            return Err(crate::BackendError::operation(error).context("failed to send backend request"));
        }
        Ok((id, receiver))
    }

    pub(crate) async fn request_value(&self, command: AppCommand) -> Result<WorkerPayload, crate::BackendError> {
        let (id, receiver) = self.send_web_request(WorkerCommand::App(command), None)?;
        let _pending = WebPendingRequest { client: &self.remote, id, subscription: false };
        receiver.await.map_err(|_| "backend worker stopped during request".to_owned())?
    }

    async fn subscribe_value(&self, command: WorkerCommand) -> Result<(u64, WorkerPayload, async_channel::Receiver<WorkerPayload>), crate::BackendError> {
        let coalesce = matches!(command, WorkerCommand::App(AppCommand::SubscribeLibraryList));
        let (event_sender, event_receiver) = if coalesce { async_channel::bounded(1) } else { async_channel::unbounded() };
        let (id, response_receiver) = self.send_web_request(command, Some(event_sender))?;
        let mut pending = WebPendingRequest { client: &self.remote, id, subscription: true };
        match response_receiver.await.unwrap_or_else(|_| Err("backend worker stopped during subscription".into())) {
            Ok(initial) => {
                pending.subscription = false;
                Ok((id, initial, event_receiver))
            }
            Err(error) => Err(error),
        }
    }

    fn cancel_web_subscription(&self, id: u64) {
        self.remote.subscriptions.borrow_mut().remove(&id);
        self.remote.subscription_commands.borrow_mut().remove(&id);
        if let Ok(bytes) = encode_worker_message(&AppWorkerClientMessage::CancelSubscription { id }) {
            let _ = self.remote.port.send(&bytes);
        }
    }
}

impl Transport {
    pub(crate) async fn request<T: LibraryReply>(&self, command: AppCommand) -> Result<T, crate::BackendError> {
        decode_worker_payload(self.request_value(command).await?)
    }

    pub(crate) async fn library_updates_transport(&self, library_id: LibraryId) -> Result<(async_channel::Receiver<library_backend::LibraryUpdate>, bool), crate::BackendError> {
        self.worker_typed_subscription(WorkerCommand::LibrarySubscription { library_id, subscription: LibrarySubscription::Updates }).await
    }

    pub(crate) async fn book_download_changes(&self, library_id: LibraryId, content_hash: crate::ContentHash) -> Result<(async_channel::Receiver<crate::DownloadState>, crate::DownloadState), crate::BackendError> {
        self.worker_typed_subscription(WorkerCommand::LibrarySubscription { library_id, subscription: LibrarySubscription::BookDownload { content_hash } }).await
    }

    pub(crate) async fn book_subscription(&self, library_id: crate::LibraryId, content_hash: crate::ContentHash) -> Result<(async_channel::Receiver<()>, ResolvedBookData), crate::BackendError> {
        self.worker_typed_subscription(WorkerCommand::LibrarySubscription { library_id, subscription: LibrarySubscription::Book { content_hash } }).await
    }

    async fn worker_typed_subscription<Event: serde::de::DeserializeOwned + Send + 'static, Reply: serde::de::DeserializeOwned>(&self, command: WorkerCommand) -> Result<(async_channel::Receiver<Event>, Reply), crate::BackendError> {
        let (subscription_id, initial, receiver) = self.subscribe_value(command).await?;
        let initial = match decode_worker_payload(initial) {
            Ok(initial) => initial,
            Err(error) => {
                self.cancel_web_subscription(subscription_id);
                return Err(error);
            }
        };
        let (sender, updates) = async_channel::unbounded();
        let subscription_backend = self.clone();
        crate::executor::spawn_detached(async move {
            while let Some(value) = next_subscription_value(&receiver, &sender).await {
                let Ok(update) = decode_worker_payload(value) else { continue };
                if sender.send(update).await.is_err() {
                    break;
                }
            }
            subscription_backend.cancel_web_subscription(subscription_id);
        });
        Ok((updates, initial))
    }

    pub(super) async fn unit_subscription(&self, command: AppCommand) -> Result<async_channel::Receiver<()>, crate::BackendError> {
        let (subscription_id, _initial, receiver) = self.subscribe_value(WorkerCommand::App(command)).await?;
        let (sender, updates) = async_channel::bounded(1);
        let subscription_backend = self.clone();
        crate::executor::spawn_detached(async move {
            while next_subscription_value(&receiver, &sender).await.is_some() {
                if matches!(sender.try_send(()), Err(async_channel::TrySendError::Closed(()))) {
                    break;
                }
            }
            subscription_backend.cancel_web_subscription(subscription_id);
        });
        Ok(updates)
    }
}

// Dropping a view's receiver cancels its worker subscription even when the
// library is idle and no further event would otherwise wake the bridge.
async fn next_subscription_value<T>(receiver: &async_channel::Receiver<WorkerPayload>, sender: &async_channel::Sender<T>) -> Option<WorkerPayload> {
    tokio::select! {
        _ = sender.closed() => None,
        value = receiver.recv() => value.ok(),
    }
}

fn restored_subscription_updates(command: &[u8], result: Result<WorkerPayload, crate::BackendError>) -> Result<Vec<WorkerPayload>, crate::BackendError> {
    let initial = result?;
    let request: AppWorkerClientMessage = decode_worker_message(command)?;
    if let AppWorkerClientMessage::Request(AppWorkerRequest { command: WorkerCommand::LibrarySubscription { subscription, .. }, .. }) = request {
        subscription.restored_updates(initial)
    } else {
        // Application subscriptions use their initial reply to signal a refresh.
        Ok(vec![initial])
    }
}

impl WebWorkerClient {
    pub(crate) fn import_updates(&self) -> tokio::sync::watch::Receiver<std::collections::BTreeMap<String, crate::ImportProgress>> {
        self._connection.import_updates()
    }
    pub(crate) fn retry_import(&self, id: &str) {
        self._connection.retry_import(id);
    }
}

impl Transport {
    pub(crate) async fn library_request(&self, library_id: LibraryId, command: crate::library::LibraryCommand) -> Result<WorkerPayload, crate::BackendError> {
        let command = WorkerCommand::Library { library_id, command: command };
        let (id, receiver) = self.send_web_request(command, None)?;
        let _pending = WebPendingRequest { client: &self.remote, id, subscription: false };
        // The outer result reports a lost worker; the inner one is the reply the
        // library actor produced.
        receiver.await.map_err(|_| "backend worker stopped during request".to_owned())?
    }
}

impl AppClient {
    pub async fn connect_web(config: crate::host::web::WebConfiguration) -> Result<(Self, AppStartupState), crate::BackendError> {
        let (transport, startup) = Transport::connect_web_transport(config).await?;
        Ok((Self { transport }, startup))
    }

    pub fn import_updates(&self) -> tokio::sync::watch::Receiver<std::collections::BTreeMap<String, crate::ImportProgress>> {
        self.transport.remote.import_updates()
    }

    pub fn retry_import(&self, id: &str) {
        self.transport.remote.retry_import(id);
    }
}

impl Transport {
    pub(crate) async fn import_selected_directory(&self, library_id: crate::LibraryId, parent_id: crate::DirId, directory: crate::DirectoryImport, create_root: bool) -> Result<Vec<crate::ImportFailure>, crate::BackendError> {
        self.remote._connection.import_selected_directory(library_id, parent_id, directory, create_root).await.map_err(crate::BackendError::operation)
    }
}
