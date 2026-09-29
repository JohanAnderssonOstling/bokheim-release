//! Process-wide foreground-interest and remote-change hints for library work.

use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default)]
pub struct InterestState {
    foreground: usize,
    resume_generation: u64,
    browsing: usize,
    work: usize,
}

impl InterestState {
    pub fn browsing(self) -> bool {
        self.browsing != 0
    }
    pub fn foreground(self) -> bool {
        self.foreground != 0
    }
    pub fn resume_generation(self) -> u64 {
        self.resume_generation
    }
    pub fn enabled(self) -> bool {
        self.foreground != 0 && (self.browsing != 0 || self.work != 0)
    }
    fn policy(self) -> (bool, bool, bool, u64) {
        (self.enabled(), self.browsing(), self.foreground(), self.resume_generation)
    }
}

#[derive(Clone, Debug)]
pub struct NotificationInterest(watch::Sender<InterestState>);

impl Default for NotificationInterest {
    fn default() -> Self {
        Self(watch::channel(InterestState::default()).0)
    }
}

pub struct InterestLease {
    interest: NotificationInterest,
    kind: InterestKind,
}
enum InterestKind {
    View(bool),
    Work,
}

impl InterestLease {
    fn update(&self, add: bool) {
        self.interest.0.send_if_modified(|state| {
            let before = state.policy();
            let change = |count: &mut usize| if add { *count += 1 } else { *count -= 1 };
            match self.kind {
                InterestKind::View(browsing) => {
                    if add && state.foreground == 0 {
                        state.resume_generation += 1;
                    }
                    change(&mut state.foreground);
                    if browsing {
                        change(&mut state.browsing);
                    }
                }
                InterestKind::Work => change(&mut state.work),
            }
            before != state.policy()
        });
    }
}

impl Drop for InterestLease {
    fn drop(&mut self) {
        self.update(false);
    }
}

impl NotificationInterest {
    fn acquire(&self, kind: InterestKind) -> InterestLease {
        let lease = InterestLease { interest: self.clone(), kind };
        lease.update(true);
        lease
    }
    pub fn view(&self, browsing: bool) -> InterestLease {
        self.acquire(InterestKind::View(browsing))
    }
    pub fn work(&self) -> InterestLease {
        self.acquire(InterestKind::Work)
    }
    pub fn subscribe(&self) -> watch::Receiver<InterestState> {
        self.0.subscribe()
    }
}

#[derive(Clone, Debug)]
pub struct RemoteChanges(watch::Sender<u64>);

impl Default for RemoteChanges {
    fn default() -> Self {
        Self(watch::channel(0).0)
    }
}

impl RemoteChanges {
    pub fn request(&self) {
        self.0.send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.subscribe()
    }
}
