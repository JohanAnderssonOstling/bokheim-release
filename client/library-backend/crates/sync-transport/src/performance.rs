//! Opt-in phase timings; never include credentials, paths or payloads.
pub struct PerformanceTrace {
    id: uuid::Uuid,
    operation: &'static str,
    started: web_time::Instant,
    phase_started: web_time::Instant,
    phase: &'static str,
    finished: bool,
}

impl PerformanceTrace {
    pub fn new(operation: &'static str, phase: &'static str) -> Self {
        let now = web_time::Instant::now();
        let trace = Self { id: uuid::Uuid::new_v4(), operation, started: now, phase_started: now, phase, finished: false };
        log::debug!(target: "sync_performance", "side=client trace_id={} operation={} event=start phase={}", trace.id, operation, phase);
        trace
    }

    pub fn id(&self) -> String {
        self.id.to_string()
    }

    pub fn phase(&mut self, next: &'static str) {
        log::debug!(target: "sync_performance", "side=client trace_id={} operation={} event=phase phase={} elapsed_ms={:.3}", self.id, self.operation, self.phase, self.phase_started.elapsed().as_secs_f64() * 1000.0);
        self.phase = next;
        self.phase_started = web_time::Instant::now();
    }

    pub fn finish(&mut self, ok: bool) {
        self.phase("finished");
        self.finished = true;
        log::debug!(target: "sync_performance", "side=client trace_id={} operation={} event=complete ok={} total_ms={:.3}", self.id, self.operation, ok, self.started.elapsed().as_secs_f64() * 1000.0);
    }
}

impl Drop for PerformanceTrace {
    fn drop(&mut self) {
        if !self.finished {
            log::debug!(target: "sync_performance", "side=client trace_id={} operation={} event=interrupted phase={} phase_ms={:.3} total_ms={:.3}", self.id, self.operation, self.phase, self.phase_started.elapsed().as_secs_f64() * 1000.0, self.started.elapsed().as_secs_f64() * 1000.0);
        }
    }
}
