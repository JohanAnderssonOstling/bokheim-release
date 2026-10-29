//! A storage request lends its owned handler to one command at a time.
use async_channel::Receiver;

type ResultBytes = Result<Vec<u8>, String>;

pub(crate) async fn serve_storage(mut run: impl AsyncFnMut(Vec<u8>) -> ResultBytes, commands: Receiver<(u32, Vec<u8>)>, completed: Receiver<ResultBytes>, mut reply: impl FnMut(u32, ResultBytes) -> Result<(), String>) -> ResultBytes {
    tokio::select! {
        biased;
        result = completed.recv() => result.unwrap_or_else(|_| Err("Coordinator request closed".into())),
        result = async {
            while let Ok((id, bytes)) = commands.recv().await {
                let result = run(bytes).await;
                reply(id, result)?;
            }
            Err("Coordinator request closed".into())
        } => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply::{send_once, OwnedReply};
    use async_channel::{bounded, unbounded};
    use std::cell::Cell;
    use std::future::pending;

    struct Dropped<'a>(&'a Cell<bool>);
    impl Drop for Dropped<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[tokio::test]
    async fn commands_lend_the_same_state_in_order_across_await() {
        let (commands, incoming) = unbounded();
        commands.try_send((1, vec![10])).unwrap();
        commands.try_send((2, vec![20])).unwrap();
        let (completed, completion) = bounded(1);
        let mut scan = Vec::new();
        let mut replies = Vec::new();
        let result = serve_storage(
            async |bytes| {
                scan.extend(bytes);
                tokio::task::yield_now().await;
                Ok(scan.clone())
            },
            incoming,
            completion,
            |id, result| {
                replies.push((id, result.unwrap()));
                if id == 2 {
                    completed.try_send(Ok(vec![30])).unwrap();
                }
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap(), vec![30]);
        assert_eq!(scan, vec![10, 20]);
        assert_eq!(replies, vec![(1, vec![10]), (2, vec![10, 20])]);
    }

    #[tokio::test]
    async fn completion_wins_over_queued_commands() {
        let (commands, incoming) = unbounded();
        commands.try_send((1, vec![])).unwrap();
        let (completed, completion) = bounded(1);
        completed.try_send(Err("cancelled".into())).unwrap();
        let result = serve_storage(async |_| panic!("cancelled command ran"), incoming, completion, |_, _| panic!("cancelled command replied")).await;
        assert_eq!(result.unwrap_err(), "cancelled");
    }

    #[tokio::test]
    async fn failed_storage_reply_cancels_the_unfinished_coordinator_request() {
        let (commands, incoming) = unbounded();
        commands.try_send((1, vec![10])).unwrap();
        commands.try_send((2, vec![20])).unwrap();
        let (_completed, completion) = bounded(1);
        let cancelled = Cell::new(false);
        let owner = OwnedReply::new(completion.clone(), || cancelled.set(true));
        let started = Cell::new(0);
        let result = serve_storage(
            async |bytes| {
                started.set(started.get() + 1);
                Ok(bytes)
            },
            incoming,
            completion,
            |_, _| Err("reply port failed".into()),
        )
        .await;
        assert_eq!(result.unwrap_err(), "reply port failed");
        assert_eq!(started.get(), 1);
        drop(owner);
        assert!(cancelled.get());
    }

    #[tokio::test]
    async fn consumed_coordinator_completion_does_not_cancel_the_request() {
        let (_commands, incoming) = unbounded();
        let (completed, completion) = bounded(1);
        let cancelled = Cell::new(false);
        let owner = OwnedReply::new(completion.clone(), || cancelled.set(true));
        send_once(&completed, Ok(vec![30]));
        let result = serve_storage(async |_| panic!("completed request ran"), incoming, completion, |_, _| panic!("completed request replied")).await;
        assert_eq!(result.unwrap(), vec![30]);
        drop(owner);
        assert!(!cancelled.get());
    }

    #[tokio::test]
    async fn completion_drops_active_operation_and_skips_remaining_commands() {
        let (commands, incoming) = unbounded();
        commands.try_send((1, vec![])).unwrap();
        commands.try_send((2, vec![])).unwrap();
        let (completed, completion) = bounded(1);
        let dropped = Cell::new(false);
        let started = Cell::new(0);
        let result = serve_storage(
            async |_| {
                started.set(started.get() + 1);
                let _guard = Dropped(&dropped);
                completed.try_send(Err("host failed".into())).unwrap();
                pending().await
            },
            incoming,
            completion,
            |_, _| panic!("interrupted command replied"),
        )
        .await;
        assert_eq!(result.unwrap_err(), "host failed");
        assert_eq!(started.get(), 1);
        assert!(dropped.get());
    }

    #[tokio::test]
    async fn dropping_request_drops_active_operation_and_owned_handler() {
        let (commands, incoming) = unbounded();
        commands.try_send((1, vec![])).unwrap();
        let (_completed, completion) = bounded(1);
        let handler_dropped = Cell::new(false);
        let operation_dropped = Cell::new(false);
        let owned = Dropped(&handler_dropped);
        let active = &operation_dropped;
        let run = async move |_| {
            let _owner = &owned;
            let _operation = Dropped(active);
            pending().await
        };
        let mut request = Box::pin(serve_storage(run, incoming, completion, |_, _| panic!("cancelled command replied")));
        assert!(futures_util::poll!(&mut request).is_pending());
        drop(request);
        assert!(handler_dropped.get());
        assert!(operation_dropped.get());
    }
}
