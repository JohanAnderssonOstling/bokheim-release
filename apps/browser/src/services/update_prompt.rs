//! A shared launch owns one availability prompt, including across windows.
#[derive(Default)]
pub(super) struct UpdatePromptState {
    shown: bool,
}

impl UpdatePromptState {
    pub(super) fn take(&mut self, available: bool) -> bool {
        if !available || self.shown {
            return false;
        }
        self.shown = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_can_finish_after_the_window_opens() {
        let mut prompt = UpdatePromptState::default();
        assert!(!prompt.take(false));
        assert!(prompt.take(true));
    }

    #[test]
    fn later_keeps_the_offer_without_reopening_on_refresh_or_another_window() {
        let mut prompt = UpdatePromptState::default();
        assert!(prompt.take(true));
        assert!(!prompt.take(true));
        assert!(!prompt.take(false));
        assert!(!prompt.take(true));
    }

    #[test]
    fn an_existing_offer_is_prompted_again_on_a_new_launch() {
        let mut old_launch = UpdatePromptState::default();
        assert!(old_launch.take(true));
        assert!(!old_launch.take(true));
        assert!(UpdatePromptState::default().take(true));
    }
}
