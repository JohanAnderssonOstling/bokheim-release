use gpui::Task;

/// Owns one replaceable page request and rejects completions from older work.
/// Dropping the previous GPUI task also drops/aborts its backend task.
#[derive(Default)]
pub(crate) struct LatestRequest {
    generation: u64,
    task: Option<Task<()>>,
}

impl LatestRequest {
    pub(crate) fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.task.take();
        self.generation
    }

    pub(crate) fn install(&mut self, task: Task<()>) {
        self.task = Some(task);
    }

    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

#[cfg(test)]
mod tests {
    use super::LatestRequest;

    #[test]
    fn only_the_latest_generation_is_current() {
        let mut request = LatestRequest::default();
        let first = request.begin();
        assert!(request.is_current(first));
        let second = request.begin();
        assert!(!request.is_current(first));
        assert!(request.is_current(second));
    }
}
