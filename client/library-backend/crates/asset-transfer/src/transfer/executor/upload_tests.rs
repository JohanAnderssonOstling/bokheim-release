use super::collect_uploads;
use crate::TransferError;
use std::{cell::Cell, future::pending, rc::Rc};
use tokio::sync::Semaphore;

struct InFlight(Rc<Cell<usize>>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[tokio::test]
async fn upload_limits_preserve_progress_after_ordinary_failures() {
    for concurrency in [1, 4] {
        let active = Rc::new(Cell::new(0));
        let peak = Rc::new(Cell::new(0));
        let completed = Rc::new(Cell::new(0));
        let gate = Rc::new(Semaphore::new(0));
        let uploads = (0..12).map(|index| {
            let (active, peak, completed, gate) = (active.clone(), peak.clone(), completed.clone(), gate.clone());
            async move {
                active.set(active.get() + 1);
                peak.set(peak.get().max(active.get()));
                let _active = InFlight(active);
                gate.acquire().await.unwrap().forget();
                completed.set(completed.get() + 1);
                match index {
                    1 => Err(TransferError::retryable("temporary failure")),
                    2 => Err(TransferError::rejected("permanent failure")),
                    _ => Ok(()),
                }
            }
        });
        let release = async {
            for _ in 0..100 {
                if active.get() == concurrency {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(active.get(), concurrency);
            gate.add_permits(12);
        };
        let (results, ()) = tokio::join!(collect_uploads(uploads, concurrency), release);
        assert_eq!(peak.get(), concurrency);
        assert_eq!(completed.get(), 12);
        assert_eq!(results.len(), 12);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 2);
        assert_eq!(active.get(), 0);
    }
}

#[tokio::test]
async fn authentication_failure_cancels_in_flight_uploads_and_skips_queued_work() {
    for concurrency in [1, 4] {
        let active = Rc::new(Cell::new(0));
        let started = Rc::new(Cell::new(0));
        let uploads = (0..12).map(|index| {
            let (active, started) = (active.clone(), started.clone());
            async move {
                active.set(active.get() + 1);
                started.set(started.get() + 1);
                let _active = InFlight(active);
                if index == 0 {
                    return Ok(());
                }
                if index != 1 {
                    return pending().await;
                }
                for _ in 0..100 {
                    if started.get() == concurrency + 1 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                Err(TransferError::AuthenticationRequired)
            }
        });
        let results = collect_uploads(uploads, concurrency).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok(), "completed uploads must survive a later authentication failure");
        assert!(matches!(results[1], Err(TransferError::AuthenticationRequired)));
        assert_eq!(started.get(), concurrency + 1);
        assert_eq!(active.get(), 0);
    }
}

#[tokio::test]
async fn cancelling_a_batch_releases_uploads_without_starting_more() {
    for concurrency in [1, 4] {
        let active = Rc::new(Cell::new(0));
        let started = Rc::new(Cell::new(0));
        let uploads = (0..12).map(|_| {
            let (active, started) = (active.clone(), started.clone());
            async move {
                active.set(active.get() + 1);
                started.set(started.get() + 1);
                let _active = InFlight(active);
                pending().await
            }
        });
        let mut batch = Box::pin(collect_uploads(uploads, concurrency));
        assert!(futures_util::poll!(&mut batch).is_pending());
        assert_eq!(active.get(), concurrency);
        drop(batch);
        assert_eq!(active.get(), 0);
        assert_eq!(started.get(), concurrency);
    }
}
