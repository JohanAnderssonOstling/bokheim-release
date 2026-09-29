//! Opt-in, native transaction phase timings. Enable with
//! `RUST_LOG=library_database::timing=debug`; no book identities are logged.

pub(crate) struct PhaseTimer {
    scope: &'static str,
    started: Option<std::time::Instant>,
}

impl PhaseTimer {
    pub(crate) fn new(scope: &'static str) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let started = log::log_enabled!(target: "library_database::timing", log::Level::Debug).then(std::time::Instant::now);
        #[cfg(target_arch = "wasm32")]
        let started = None;
        Self { scope, started }
    }

    /// Report disjoint phases within this scope. Reset after logging so log
    /// output overhead is not attributed to the next phase.
    pub(crate) fn mark(&mut self, phase: &'static str) {
        if let Some(started) = self.started {
            log::debug!(target: "library_database::timing", "scope={} phase={} ms={:.3}", self.scope, phase, started.elapsed().as_secs_f64() * 1000.0);
            self.started = Some(std::time::Instant::now());
        }
    }
}
