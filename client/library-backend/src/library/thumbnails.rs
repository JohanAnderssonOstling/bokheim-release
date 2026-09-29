//! Library-owned thumbnail-work coordination.

use std::sync::Mutex;

use library_replica::ScanProgress;

#[derive(Default)]
pub struct ThumbnailWork {
    state: Mutex<ThumbnailWorkState>,
}

#[derive(Default)]
struct ThumbnailWorkState {
    batch_active: bool,
    generations_running: usize,
    progress: ScanProgress,
}

impl ThumbnailWork {
    pub fn begin_batch(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.batch_active {
            return false;
        }
        state.batch_active = true;
        state.progress = ScanProgress::default();
        true
    }
    pub fn finish_batch(&self) {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).batch_active = false;
    }
    pub fn begin_generation(&self) {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).generations_running += 1;
    }
    pub fn finish_generation(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.generations_running = state.generations_running.checked_sub(1).expect("thumbnail generation activity must be balanced");
    }
    pub fn running(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.batch_active || state.generations_running > 0
    }
    pub fn progress(&self) -> Option<ScanProgress> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        (state.batch_active || state.generations_running > 0).then_some(state.progress)
    }
    pub fn waiting(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        (state.batch_active || state.generations_running > 0) && state.generations_running == 0
    }
    pub fn prepare_batch(&self, pending: u64) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !state.batch_active && state.generations_running == 0 {
            state.progress = ScanProgress::default();
        }
        state.progress.total = state.progress.total.max(state.progress.succeeded + pending);
    }
    pub fn complete_item(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.progress.succeeded += 1;
        state.progress.total = state.progress.total.max(state.progress.succeeded);
    }
    pub fn generations_running(&self) -> usize {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).generations_running
    }
}
