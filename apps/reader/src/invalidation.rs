use gpui::Context;

/// An entity-level UI boundary and the unit the debug counters group by.
///
/// One variant per entity: two names for the same entity would split its
/// counts across two rows and hide how often it actually repaints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Component {
    /// The EPUB window: document, sidebars and overlays.
    ReaderShell,
    /// The PDF window.
    PdfShell,
}

impl Component {
    pub fn name(self) -> &'static str {
        match self {
            Self::ReaderShell => "reader_shell",
            Self::PdfShell => "pdf_shell",
        }
    }
}

#[cfg(debug_assertions)]
use std::collections::HashMap;
#[cfg(debug_assertions)]
use std::sync::{Mutex, OnceLock};

#[cfg(debug_assertions)]
#[derive(Clone, Debug, Default)]
pub struct ReaderPerformanceSnapshot {
    pub notifications: Vec<((Component, &'static str), u64)>,
    pub renders: Vec<(Component, u64)>,
}

/// Request a reader repaint and retain a debug-only component/reason count.
pub(crate) fn notify<T: 'static>(cx: &mut Context<T>, component: Component, reason: &'static str) {
    #[cfg(debug_assertions)]
    {
        let counters = NOTIFICATION_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut counters = counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *counters.entry((component, reason)).or_default() += 1;
    }
    cx.notify();
}

#[cfg(debug_assertions)]
static NOTIFICATION_COUNTS: OnceLock<Mutex<HashMap<(Component, &'static str), u64>>> = OnceLock::new();

#[cfg(debug_assertions)]
static RENDER_COUNTS: OnceLock<Mutex<HashMap<Component, u64>>> = OnceLock::new();

#[cfg(debug_assertions)]
pub(crate) fn record_render(component: Component) {
    let counters = RENDER_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut counters = counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *counters.entry(component).or_default() += 1;
}

#[cfg(not(debug_assertions))]
pub(crate) fn record_render(_component: Component) {}

#[cfg(debug_assertions)]
pub(crate) fn snapshot() -> Vec<((Component, &'static str), u64)> {
    let mut snapshot: Vec<((Component, &'static str), u64)> = NOTIFICATION_COUNTS.get().and_then(|counts| counts.lock().ok().map(|counts| counts.iter().map(|(key, count)| (*key, *count)).collect())).unwrap_or_default();
    snapshot.sort_unstable_by_key(|(key, _)| *key);
    snapshot
}

#[cfg(debug_assertions)]
pub(crate) fn performance_snapshot() -> ReaderPerformanceSnapshot {
    let notifications = snapshot();
    let mut renders: Vec<(Component, u64)> = RENDER_COUNTS.get().and_then(|counts| counts.lock().ok().map(|counts| counts.iter().map(|(component, count)| (*component, *count)).collect())).unwrap_or_default();
    renders.sort_unstable_by_key(|(component, _)| *component);
    ReaderPerformanceSnapshot { notifications, renders }
}

#[cfg(debug_assertions)]
pub(crate) fn reset_performance_measurements() {
    if let Some(counts) = NOTIFICATION_COUNTS.get() {
        counts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }
    if let Some(counts) = RENDER_COUNTS.get() {
        counts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }
}
