//! Opt-in timing of native folder selection. No paths or book data are logged.
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};
use sync_common::LibraryId;
use web_time::Instant;

#[derive(Clone, Default)]
pub(crate) struct Trace(Option<Rc<State>>);
struct State {
    id: uuid::Uuid,
    started: Instant,
    stages: RefCell<HashSet<&'static str>>,
}
thread_local! {
    static EPOCH: Instant = Instant::now();
    static LIBRARIES: RefCell<HashMap<LibraryId, HashSet<&'static str>>> = RefCell::new(HashMap::new());
}
fn at_ms() -> f64 {
    EPOCH.with(|epoch| epoch.elapsed().as_secs_f64() * 1000.0)
}
impl Trace {
    pub(crate) fn start() -> Self {
        if !log::log_enabled!(target: "library_add_latency", log::Level::Debug) {
            return Self::default();
        }
        let trace = Self(Some(Rc::new(State { id: uuid::Uuid::new_v4(), started: Instant::now(), stages: RefCell::new(HashSet::new()) })));
        trace.mark("picker_returned");
        trace
    }
    pub(crate) fn mark(&self, stage: &'static str) -> bool {
        let Some(state) = &self.0 else {
            return false;
        };
        if !state.stages.borrow_mut().insert(stage) {
            return false;
        }
        log::debug!(target: "library_add_latency", "ui_trace={} stage={} at_ms={:.3} elapsed_ms={:.3}", state.id, stage, at_ms(), state.started.elapsed().as_secs_f64() * 1000.0);
        true
    }
    pub(crate) fn attach(&self, library: LibraryId) {
        let Some(state) = &self.0 else {
            return;
        };
        log::debug!(target: "library_add_latency", "ui_trace={} library_id={} stage=attach at_ms={:.3} elapsed_ms={:.3}", state.id, library, at_ms(), state.started.elapsed().as_secs_f64()*1000.0);
    }
}
/// Library markers may precede the add response when list notifications select
/// an initially empty app's first library. Shared at_ms preserves that ordering.
pub(crate) fn mark(library: LibraryId, stage: &'static str) -> bool {
    if !log::log_enabled!(target: "library_add_latency", log::Level::Debug) {
        return false;
    }
    LIBRARIES.with(|traces| {
        let mut traces = traces.borrow_mut();
        if traces.len() >= 64 && !traces.contains_key(&library) {
            traces.clear();
        }
        let stages = traces.entry(library).or_default();
        if stage == "session_creation_started" {
            stages.clear();
        }
        if !stages.insert(stage) {
            return false;
        }
        log::debug!(target: "library_add_latency", "library_id={} stage={} at_ms={:.3}", library, stage, at_ms());
        true
    })
}
