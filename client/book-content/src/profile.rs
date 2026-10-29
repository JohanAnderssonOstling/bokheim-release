//! Request-scoped diagnostics for reading preparation, without logging.

use std::{
    future::{Future, poll_fn},
    pin::pin,
    time::{Duration, Instant},
};

/// Wall time and time spent inside the future's poll calls. Suspended time
/// includes I/O waits and executor scheduling; active time is not CPU usage.
#[derive(Clone, Debug, Default)]
pub struct PreparationTiming {
    pub elapsed: Duration,
    pub active: Duration,
    pub polls: u64,
}

impl PreparationTiming {
    pub fn suspended(&self) -> Duration {
        self.elapsed.saturating_sub(self.active)
    }
}

#[derive(Debug)]
pub struct PreparationStage {
    pub name: &'static str,
    pub timing: PreparationTiming,
}

#[derive(Debug, Default)]
pub struct ResourceReadTiming {
    pub open: PreparationTiming,
    pub allocate: PreparationTiming,
    pub decode: PreparationTiming,
    pub compressed_bytes: u64,
    pub output_bytes: usize,
    pub output_capacity: usize,
}

#[derive(Debug)]
pub struct ResourcePreparation {
    pub uri: String,
    pub timing: ResourceReadTiming,
}

/// Nested stages overlap. Resource timings are contained by `text_resources`.
#[derive(Debug, Default)]
pub struct PreparationProfile {
    pub total: PreparationTiming,
    pub stages: Vec<PreparationStage>,
    pub resources: Vec<ResourcePreparation>,
}

impl PreparationProfile {
    pub(crate) fn record_sync(&mut self, name: &'static str, started: Option<Instant>) {
        if let Some(started) = started {
            let elapsed = started.elapsed();
            self.record(name, PreparationTiming { elapsed, active: elapsed, polls: 1 });
        }
    }

    pub(crate) fn record(&mut self, name: &'static str, timing: PreparationTiming) {
        self.stages.push(PreparationStage { name, timing });
    }
}

pub(crate) async fn measure<const PROFILE: bool, F: Future>(future: F) -> (F::Output, PreparationTiming) {
    if !PROFILE {
        return (future.await, PreparationTiming::default());
    }
    let started = Instant::now();
    let mut active = Duration::ZERO;
    let mut polls = 0;
    let mut future = pin!(future);
    let output = poll_fn(|cx| {
        let polled = Instant::now();
        let result = future.as_mut().poll(cx);
        active += polled.elapsed();
        polls += 1;
        result
    })
    .await;
    (output, PreparationTiming { elapsed: started.elapsed(), active, polls })
}
