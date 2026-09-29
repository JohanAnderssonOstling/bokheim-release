//! A connection owns its endpoint and its startup cancellation handle together.
pub struct Connection<T, C> {
    pub endpoint: T,
    generation: u64,
    startup: Option<C>,
}
impl<T, C> Connection<T, C> {
    pub fn new(endpoint: T, generation: u64, startup: C) -> Self {
        Self { endpoint, generation, startup: Some(startup) }
    }
    pub fn accepts(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub fn waiting(&self, generation: u64) -> bool {
        self.accepts(generation) && self.startup.is_some()
    }
    pub fn ready(&mut self, generation: u64) {
        if self.accepts(generation) {
            self.startup.take();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    #[test]
    fn stale_readiness_cannot_cancel_replacement_deadline() {
        let timer = Rc::new(());
        let mut worker = Connection::new((), 2, timer.clone());
        worker.ready(1);
        assert!(!worker.accepts(1));
        assert!(!worker.waiting(1));
        assert!(worker.waiting(2));
        assert_eq!(Rc::strong_count(&timer), 2);
        worker.ready(2);
        assert!(!worker.waiting(2));
        assert_eq!(Rc::strong_count(&timer), 1);
        assert!(worker.accepts(2));
    }
    #[test]
    fn stopping_a_connection_releases_endpoint_and_startup_together() {
        let endpoint = Rc::new(());
        let timer = Rc::new(());
        let worker = Connection::new(endpoint.clone(), 1, timer.clone());
        drop(worker);
        assert_eq!(Rc::strong_count(&endpoint), 1);
        assert_eq!(Rc::strong_count(&timer), 1);
    }
}
