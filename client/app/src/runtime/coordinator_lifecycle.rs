//! Coordinator lifecycle decisions, independent of browser workers and ports.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Connect { id: u32 },
    Reject { id: u32, error: String },
    StopBackground,
    FailClients { error: String },
    FailNetwork { error: String },
    FailSync { error: String },
    FailTransfers { error: String },
    FailCpu { error: String },
    NotifySyncFailure { error: String },
    StopNetwork,
    StopBackend,
    Close,
}

#[derive(Default)]
enum State {
    #[default]
    Starting,
    Ready,
    Failed(String),
}

#[derive(Default)]
pub struct CoordinatorLifecycle {
    state: State,
    waiting: Vec<u32>,
}

impl CoordinatorLifecycle {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn starting(&self) -> bool {
        matches!(self.state, State::Starting)
    }
    pub fn accepting_work(&self) -> bool {
        !matches!(self.state, State::Failed(_))
    }

    pub fn connect(&mut self, id: u32) -> Vec<Action> {
        match &self.state {
            State::Starting => {
                self.waiting.push(id);
                vec![]
            }
            State::Ready => vec![Action::Connect { id }],
            State::Failed(error) => vec![Action::Reject { id, error: error.clone() }],
        }
    }
    pub fn ready(&mut self) -> Vec<Action> {
        if !matches!(self.state, State::Starting) {
            return vec![];
        }
        self.state = State::Ready;
        self.waiting.drain(..).map(|id| Action::Connect { id }).collect()
    }
    pub fn fail(&mut self, error: String) -> Vec<Action> {
        if matches!(self.state, State::Failed(_)) {
            return vec![];
        }
        self.state = State::Failed(error.clone());
        let mut result = vec![
            Action::StopBackground,
            Action::FailClients { error: error.clone() },
            Action::FailNetwork { error: error.clone() },
            Action::FailSync { error: error.clone() },
            Action::FailTransfers { error: error.clone() },
            Action::NotifySyncFailure { error: error.clone() },
            Action::FailCpu { error: error.clone() },
            Action::StopNetwork,
        ];
        result.extend(self.waiting.drain(..).map(|id| Action::Reject { id, error: error.clone() }));
        // Release SQLite/OPFS ownership before closing the coordinator.
        result.extend([Action::StopBackend, Action::Close]);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_drains_waiters_and_cannot_revive_failed_coordinator() {
        let mut c = CoordinatorLifecycle::new();
        assert!(c.accepting_work());
        assert!(c.connect(1).is_empty());
        assert!(c.connect(2).is_empty());
        assert_eq!(c.ready(), vec![Action::Connect { id: 1 }, Action::Connect { id: 2 }]);
        assert!(c.ready().is_empty());
        assert_eq!(c.connect(3), vec![Action::Connect { id: 3 }]);
        let error = "stopped".to_owned();
        assert_eq!(
            c.fail(error.clone()),
            vec![
                Action::StopBackground,
                Action::FailClients { error: error.clone() },
                Action::FailNetwork { error: error.clone() },
                Action::FailSync { error: error.clone() },
                Action::FailTransfers { error: error.clone() },
                Action::NotifySyncFailure { error: error.clone() },
                Action::FailCpu { error: error.clone() },
                Action::StopNetwork,
                Action::StopBackend,
                Action::Close,
            ]
        );
        assert!(!c.accepting_work());
        assert!(c.ready().is_empty());
        assert!(c.fail("again".into()).is_empty());
        assert_eq!(c.connect(4), vec![Action::Reject { id: 4, error }]);
    }

    #[test]
    fn startup_failure_rejects_waiters_before_closing_the_worker() {
        let mut c = CoordinatorLifecycle::new();
        c.connect(1);
        c.connect(2);
        let actions = c.fail("startup failed".into());
        assert_eq!(&actions[actions.len() - 4..], &[Action::Reject { id: 1, error: "startup failed".into() }, Action::Reject { id: 2, error: "startup failed".into() }, Action::StopBackend, Action::Close,]);
        assert!(c.ready().is_empty());
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    #[test]
    fn startup_deadline_applies_only_before_readiness() {
        let mut coordinator = CoordinatorLifecycle::new();
        assert!(coordinator.starting());
        coordinator.connect(1);
        let actions = coordinator.fail("startup timed out".into());
        assert!(!coordinator.starting());
        assert!(actions.contains(&Action::Reject { id: 1, error: "startup timed out".into() }));
        assert!(coordinator.ready().is_empty());
        let mut replacement = CoordinatorLifecycle::new();
        replacement.ready();
        assert!(!replacement.starting());
    }
}
