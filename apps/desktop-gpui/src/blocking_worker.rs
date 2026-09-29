//! Desktop host support for work that must not block GPUI's event thread.

use gpui::BackgroundExecutor;

type BlockingJob = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
pub(crate) struct BlockingWorker {
    jobs: async_channel::Sender<BlockingJob>,
}

impl BlockingWorker {
    pub(crate) fn new(executor: &BackgroundExecutor, thread_count: usize) -> Self {
        let (jobs, receiver) = async_channel::unbounded::<BlockingJob>();
        for _ in 0..thread_count.max(1) {
            let receiver = receiver.clone();
            executor
                .scheduler_executor()
                .spawn_dedicated(move |_executor| async move {
                    while let Ok(job) = receiver.recv().await {
                        job();
                    }
                })
                .detach();
        }
        Self { jobs }
    }

    pub(crate) fn dispatch<T>(&self, work: impl FnOnce() -> T + Send + 'static) -> async_channel::Receiver<T>
    where
        T: Send + 'static,
    {
        let (result_tx, result_rx) = async_channel::bounded(1);
        let job = Box::new(move || {
            if result_tx.is_closed() {
                return;
            }
            let result = work();
            let _ = result_tx.send_blocking(result);
        });
        assert!(self.jobs.try_send(job).is_ok(), "GPUI blocking worker stopped unexpectedly");
        result_rx
    }
}
