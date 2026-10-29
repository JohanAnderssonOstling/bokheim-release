//! One sync pass owns its storage mailbox and pending reply. Browser callbacks
//! supply events; the application supplies the engine and wire sender.
use async_channel::{Receiver, Sender};
use client_platform_runtime::pending::OnDrop;
use futures_channel::oneshot;
use futures_util::future::LocalBoxFuture;
use std::cell::Cell;
use std::rc::Rc;

pub type ResultBytes = Result<Vec<u8>, String>;
pub type Operation = Rc<dyn Fn(Vec<u8>) -> LocalBoxFuture<'static, ResultBytes>>;

pub enum Event {
    Request { id: u32, bytes: Vec<u8>, reply: oneshot::Sender<ResultBytes> },
    Cancel(u32),
    Reply { id: u32, result: ResultBytes },
    Failed(String),
    Malformed,
}

fn operation(events: Sender<Event>) -> Operation {
    let next = Cell::new(0u32);
    Rc::new(move |bytes| {
        let Some(id) = next.get().checked_add(1) else {
            return Box::pin(async { Err("Sync request IDs exhausted".into()) });
        };
        next.set(id);
        let (reply, response) = oneshot::channel();
        let _ = events.try_send(Event::Request { id, bytes, reply });
        // Capture cleanup before constructing the future: an unpolled call
        // can be cancelled without starting its queued storage command.
        let cancelled = events.clone();
        let registration = OnDrop::new(move || {
            let _ = cancelled.try_send(Event::Cancel(id));
        });
        Box::pin(async move {
            let _registration = registration;
            response.await.unwrap_or_else(|_| Err("Sync storage connection closed".into()))
        })
    })
}

type Pending = Option<(u32, oneshot::Sender<ResultBytes>)>;

fn take_matching(pending: &mut Pending, id: u32) -> Option<oneshot::Sender<ResultBytes>> {
    if pending.as_ref().is_some_and(|(active, _)| *active == id) { pending.take().map(|(_, reply)| reply) } else { None }
}

pub async fn run(events: Receiver<Event>, commands: Sender<Event>, engine: impl AsyncFnOnce(Operation) -> ResultBytes, mut send: impl FnMut(u32, Vec<u8>) -> Result<(), String>) -> ResultBytes {
    let engine = engine(operation(commands));
    futures_util::pin_mut!(engine);
    let mut pending: Pending = None;
    let result = loop {
        tokio::select! {
            biased;
            event = events.recv() => match event {
                Ok(Event::Failed(error)) => break Err(error),
                Err(_) => break Err("Sync storage connection closed".into()),
                Ok(Event::Malformed) => {
                    if let Some((_, reply)) = pending.take() {
                        let _ = reply.send(Err("Invalid sync storage response".into()));
                    }
                }
                Ok(Event::Cancel(id)) => { drop(take_matching(&mut pending, id)); }
                Ok(Event::Reply { id, result }) => {
                    if let Some(reply) = take_matching(&mut pending, id) {
                        let _ = reply.send(result);
                    }
                }
                Ok(Event::Request { id, bytes, reply }) => {
                    if reply.is_canceled() {
                        continue;
                    }
                    if pending.as_ref().is_some_and(|(_, reply)| reply.is_canceled()) {
                        pending.take();
                    }
                    if pending.is_some() {
                        let _ = reply.send(Err("Sync storage command already active".into()));
                        continue;
                    }
                    pending = Some((id, reply));
                    if let Err(error) = send(id, bytes) {
                        if let Some(reply) = take_matching(&mut pending, id) {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
            },
            result = &mut engine => break result,
        }
    };
    events.close();
    drop(pending);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_channel::unbounded;
    use futures_util::future::{join, pending};

    #[tokio::test]
    async fn sequential_commands_ignore_stale_replies_and_cancellations() {
        let (commands, events) = unbounded();
        let replies = commands.clone();
        let mut sent = Vec::new();
        let result = run(
            events,
            commands,
            async |storage| {
                let mut result = storage(vec![1]).await?;
                result.extend(storage(vec![2]).await?);
                Ok(result)
            },
            |id, bytes| {
                sent.push((id, bytes.clone()));
                replies.try_send(Event::Cancel(id - 1)).unwrap();
                replies.try_send(Event::Reply { id: id - 1, result: Err("stale".into()) }).unwrap();
                replies.try_send(Event::Reply { id, result: Ok(bytes) }).unwrap();
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap(), vec![1, 2]);
        assert_eq!(sent, vec![(1, vec![1]), (2, vec![2])]);
    }

    #[tokio::test]
    async fn dropping_an_unpolled_storage_call_skips_its_command() {
        let (commands, events) = unbounded();
        let replies = commands.clone();
        let result = run(
            events,
            commands,
            async |storage| {
                drop(storage(vec![1]));
                storage(vec![2]).await
            },
            |id, bytes| {
                assert_eq!(id, 2);
                assert_eq!(bytes, vec![2]);
                replies.try_send(Event::Reply { id, result: Ok(bytes) }).unwrap();
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap(), vec![2]);
    }

    struct Dropped<'a>(&'a Cell<bool>);
    impl Drop for Dropped<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[tokio::test]
    async fn failure_interrupts_an_active_engine_and_releases_its_reply() {
        let (commands, events) = unbounded();
        let replies = commands.clone();
        let dropped = Cell::new(false);
        let result = run(
            events,
            commands,
            async |storage| {
                let _engine = Dropped(&dropped);
                storage(vec![1]).await
            },
            |_, _| {
                replies.try_send(Event::Failed("host stopped".into())).unwrap();
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap_err(), "host stopped");
        assert!(dropped.get());
    }

    #[tokio::test]
    async fn malformed_and_send_errors_settle_the_active_call() {
        let (commands, events) = unbounded();
        let replies = commands.clone();
        let result = run(
            events,
            commands,
            async |storage| {
                assert!(storage(vec![1]).await.unwrap_err().contains("Invalid sync storage"));
                assert_eq!(storage(vec![2]).await.unwrap_err(), "send failed");
                storage(vec![3]).await
            },
            |id, bytes| {
                match id {
                    1 => {
                        replies.try_send(Event::Malformed).unwrap();
                    }
                    2 => return Err("send failed".into()),
                    _ => {
                        replies.try_send(Event::Reply { id, result: Ok(bytes) }).unwrap();
                    }
                }
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap(), vec![3]);
    }

    #[tokio::test]
    async fn concurrent_storage_calls_are_rejected_without_overwriting_the_reply() {
        let (commands, events) = unbounded();
        let replies = commands.clone();
        let result = run(
            events,
            commands,
            async |storage| {
                let (first, second) = join(storage(vec![1]), storage(vec![2])).await;
                assert_eq!(second.unwrap_err(), "Sync storage command already active");
                first
            },
            |id, bytes| {
                assert_eq!(id, 1);
                replies.try_send(Event::Reply { id, result: Ok(bytes) }).unwrap();
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn dropping_the_pass_drops_the_engine_and_closes_the_mailbox() {
        let (commands, events) = unbounded();
        let observed = commands.clone();
        let dropped = Cell::new(false);
        let mut pass = Box::pin(run(
            events,
            commands,
            async |_| {
                let _engine = Dropped(&dropped);
                pending().await
            },
            |_, _| panic!("unused sender"),
        ));
        assert!(futures_util::poll!(&mut pass).is_pending());
        drop(pass);
        assert!(dropped.get());
        assert!(observed.is_closed());
    }
}
