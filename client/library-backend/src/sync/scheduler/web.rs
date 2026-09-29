use super::*;
use client_platform_web::transport::bridge::BackgroundHost;

pub(super) async fn run<C, F>(mut cycle: C, requests: async_channel::Receiver<SyncRequest>, retry: Duration, debounce: Duration)
where
    C: FnMut(bool) -> F,
    F: Future<Output = Result<(), ()>>,
{
    let scheduler = match BackgroundHost::new(retry.as_millis() as f64, debounce.as_millis() as f64) {
        Ok(scheduler) => scheduler,
        Err(error) => {
            log::error!("SharedWorker background scheduler unavailable: {error:?}");
            return;
        }
    };
    let signals = async {
        while let Ok(request) = requests.recv().await {
            match request {
                SyncRequest::Wake(_) => scheduler.notify("wake"),
                SyncRequest::WakeAfter(delay) => scheduler.wake_after(delay.as_secs_f64() * 1000.0),
                SyncRequest::RefreshRemote => scheduler.notify("refresh"),
                SyncRequest::Stop => break,
            }
        }
    };
    let cycles = async {
        loop {
            let command = match scheduler.next_cycle().await {
                Ok(command) => command,
                Err(error) => {
                    log::warn!("SharedWorker background scheduler stopped: {error}");
                    break;
                }
            };
            let result = cycle(command).await;
            let _ = scheduler.finish(result.is_ok());
        }
    };
    tokio::select! {
        biased;
        _ = signals => {},
        _ = cycles => {},
    }
}
