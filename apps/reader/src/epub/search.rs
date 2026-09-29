//! Find-in-book state and the request number that keeps an obsolete whole-book
//! scan from overwriting a newer query.
//!
//! Every keystroke supersedes the scan in flight. The worker checks the shared
//! request number to stop early, and the UI checks it again before displaying
//! the completed result set.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use html_view_core::{SearchOptions, SearchScope};

use crate::epub::SearchState;

impl Default for SearchState {
    fn default() -> Self {
        Self { query: None, pending: false, cancellation: Arc::new(AtomicU64::new(0)), options: SearchOptions { match_case: false, whole_word: false, match_diacritics: false, scope: SearchScope::WholeBook }, match_position: None }
    }
}

impl SearchState {
    /// The request currently allowed to produce results.
    pub(in crate::epub) fn current_request(&self) -> u64 {
        self.cancellation.load(Ordering::Acquire)
    }

    pub(in crate::epub) fn is_current(&self, request: u64) -> bool {
        self.current_request() == request
    }

    /// Whether completed results still belong to the active query.
    pub(in crate::epub) fn still_wants(&self, request: u64, query: &str) -> bool {
        self.is_current(request) && self.query.as_deref() == Some(query)
    }

    /// Starts a new attempt, superseding anything in flight.
    fn begin_attempt(&self) -> u64 {
        self.cancellation.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }

    /// Records newly typed text and returns its request number.
    pub(in crate::epub) fn set_query(&mut self, query: String) -> Option<u64> {
        *self.query.as_mut()? = query;
        self.pending = true;
        Some(self.begin_attempt())
    }

    /// Changes the filters and forces the current query to be searched again,
    /// since different filters yield different matches for the same text.
    pub(in crate::epub) fn update_options(&mut self, update: impl FnOnce(&mut SearchOptions)) {
        update(&mut self.options);
        self.pending = true;
        self.begin_attempt();
    }

    /// Claims the pending query.
    ///
    /// Returns `None` when the renderer is already showing this query, which
    /// is how a second Enter press falls through to "go to next match".
    pub(in crate::epub) fn take_pending(&mut self) -> Option<(String, SearchOptions)> {
        let query = self.query.as_ref()?;
        if !self.pending {
            return None;
        }
        self.pending = false;
        Some((query.clone(), self.options))
    }

    /// Clears the search and cancels anything in flight.
    pub(in crate::epub) fn clear(&mut self) {
        self.begin_attempt();
        self.query = None;
        self.pending = false;
        self.match_position = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SearchState {
        SearchState { query: Some(String::new()), ..SearchState::default() }
    }

    #[test]
    fn a_newer_query_supersedes_the_one_in_flight() {
        let mut search = state();
        let first = search.set_query("plough".to_owned()).expect("search is open");
        let second = search.set_query("ploughman".to_owned()).expect("search is open");

        assert_ne!(first, second);
        assert!(!search.is_current(first), "the earlier attempt must be recognized as stale");
        assert!(search.is_current(second));
        assert_eq!(search.current_request(), second, "the worker must see the newest request");
    }

    #[test]
    fn applying_the_same_query_twice_yields_nothing_the_second_time() {
        let mut search = state();
        search.set_query("plough".to_owned()).expect("search is open");

        assert!(search.take_pending().is_some());
        assert!(search.take_pending().is_none(), "an unchanged query must fall through to match navigation");
    }

    #[test]
    fn changing_a_filter_forces_the_same_query_to_be_searched_again() {
        let mut search = state();
        search.set_query("plough".to_owned()).expect("search is open");
        search.take_pending().expect("first apply");

        search.update_options(|options| options.match_case = true);

        let (query, options) = search.take_pending().expect("a filter change must re-apply the query");
        assert_eq!(query, "plough");
        assert!(options.match_case);
    }

    #[test]
    fn scope_selects_the_matching_renderer_option() {
        let mut search = state();
        assert_eq!(search.options.scope, SearchScope::WholeBook);

        search.update_options(|options| options.scope = SearchScope::CurrentDocument);
        assert_eq!(search.take_pending().expect("scope change must be pending").1.scope, SearchScope::CurrentDocument);
    }

    #[test]
    fn clearing_cancels_the_scan_and_drops_results() {
        let mut search = state();
        search.set_query("plough".to_owned()).expect("search is open");
        search.match_position = Some((2, 9));
        let before = search.current_request();

        search.clear();

        assert_eq!(search.query, None);
        assert_eq!(search.match_position, None);
        assert!(!search.is_current(before), "an in-flight scan must not survive a clear");
    }

    #[test]
    fn results_belong_only_to_the_query_that_asked_for_them() {
        let mut search = state();
        let stale = search.set_query("plough".to_owned()).expect("search is open");
        search.set_query("ploughman".to_owned()).expect("search is open");

        assert!(!search.still_wants(stale, "plough"), "results for a superseded query must be discarded");
        assert!(search.still_wants(search.current_request(), "ploughman"));
    }
}
