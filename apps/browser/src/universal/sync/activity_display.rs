//! Presentation timing only: backend work and its progress continue immediately.
use super::LibraryActivitySummary;
use app::TransferJobKind;
use std::time::Duration;
use sync_common::LibraryId;
use web_time::Instant;

const SHOW_DELAY: Duration = Duration::from_millis(300);
const IDLE_GRACE: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivityKind {
    StorageQuota,
    Operation,
    OperationThumbnails,
    OperationBookUploads,
    OperationCoverUploads,
    OperationSyncChanges,
    Files,
    Scan,
    Thumbnails,
    Metadata,
    Preparation,
    Transfer(TransferJobKind),
}
impl ActivityKind {
    fn server(self) -> bool {
        !matches!(self, Self::Operation | Self::OperationThumbnails | Self::Files | Self::Scan | Self::Thumbnails)
    }
}

struct Row {
    library: LibraryId,
    kind: ActivityKind,
    desired: Option<LibraryActivitySummary>,
    visible: Option<LibraryActivitySummary>,
    transition: Instant,
}

#[derive(Default)]
pub(super) struct ActivityDisplay {
    rows: Vec<Row>,
}

impl ActivityDisplay {
    pub(super) fn update(&mut self, mut incoming: Vec<(LibraryId, LibraryActivitySummary)>, now: Instant) {
        // A failed batch may coexist with a running batch of the same kind.
        // Give its actionable state priority within that stable display slot.
        let mut unique: Vec<(LibraryId, LibraryActivitySummary)> = Vec::new();
        for (library, summary) in incoming.drain(..) {
            if let Some((_, previous)) = unique.iter_mut().find(|(id, row)| *id == library && row.kind == summary.kind) {
                if summary.urgent {
                    *previous = summary;
                }
            } else {
                unique.push((library, summary));
            }
        }
        for row in &mut self.rows {
            let desired = unique.iter().position(|(id, summary)| *id == row.library && summary.kind == row.kind).map(|index| unique.remove(index).1);
            if row.desired.as_ref().map(|value| value.state) != desired.as_ref().map(|value| value.state) {
                row.transition = now;
            }
            row.desired = desired;
        }
        self.rows.extend(unique.into_iter().map(|(library, summary)| Row { library, kind: summary.kind, desired: Some(summary), visible: None, transition: now }));
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for row in &mut self.rows {
            let next = match (&row.desired, &row.visible) {
                (Some(desired), visible) if desired.urgent || visible.as_ref().is_some_and(|visible| visible.state == desired.state) || now >= row.transition + SHOW_DELAY => Some(desired.clone()),
                (None, _) if now >= row.transition + IDLE_GRACE => None,
                _ => row.visible.clone(),
            };
            changed |= next != row.visible;
            row.visible = next;
        }
        self.rows.retain(|row| row.desired.is_some() || row.visible.is_some());
        changed
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.rows
            .iter()
            .filter_map(|row| match (&row.desired, &row.visible) {
                (Some(desired), visible) if visible.as_ref().is_none_or(|visible| visible.state != desired.state) => Some(row.transition + SHOW_DELAY),
                (None, Some(_)) => Some(row.transition + IDLE_GRACE),
                _ => None,
            })
            .min()
    }

    pub(super) fn rows(&self, library: LibraryId, server: bool) -> Vec<LibraryActivitySummary> {
        self.rows.iter().filter(|row| row.library == library && row.kind.server() == server).filter_map(|row| row.visible.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::ActivityState;
    use super::*;
    fn summary(kind: ActivityKind, urgent: bool) -> LibraryActivitySummary {
        LibraryActivitySummary { kind, urgent, state: if urgent { ActivityState::Waiting } else { ActivityState::Running }, text: "Work".into(), count: None, progress: None }
    }

    #[test]
    fn brief_work_never_appears_and_idle_rows_expire_without_backend_events() {
        let now = Instant::now();
        let id = LibraryId::new_v4();
        let mut display = ActivityDisplay::default();
        display.update(vec![(id, summary(ActivityKind::Metadata, false))], now);
        assert!(!display.advance(now));
        assert_eq!(display.next_deadline(), Some(now + SHOW_DELAY));
        display.update(vec![], now + Duration::from_millis(100));
        assert!(!display.advance(now + SHOW_DELAY));
        assert_eq!(display.next_deadline(), None);
        display.update(vec![(id, summary(ActivityKind::Metadata, false))], now);
        assert!(display.advance(now + SHOW_DELAY));
        display.update(vec![], now + SHOW_DELAY);
        assert!(!display.advance(now + SHOW_DELAY));
        assert_eq!(display.next_deadline(), Some(now + SHOW_DELAY + IDLE_GRACE));
        assert!(display.advance(now + SHOW_DELAY + IDLE_GRACE));
        assert!(display.rows(id, true).is_empty());
    }

    #[test]
    fn progress_does_not_restart_show_delay_and_brief_waiting_does_not_flash() {
        let now = Instant::now();
        let id = LibraryId::new_v4();
        let mut display = ActivityDisplay::default();
        let mut work = summary(ActivityKind::Thumbnails, false);
        display.update(vec![(id, work.clone())], now);
        work.progress = Some(0.5);
        display.update(vec![(id, work.clone())], now + Duration::from_millis(200));
        display.advance(now + SHOW_DELAY);
        assert_eq!(display.rows(id, false)[0].progress, Some(0.5));
        work.state = ActivityState::Waiting;
        display.update(vec![(id, work.clone())], now + SHOW_DELAY);
        assert!(!display.advance(now + SHOW_DELAY));
        work.state = ActivityState::Running;
        display.update(vec![(id, work.clone())], now + SHOW_DELAY + Duration::from_millis(100));
        assert!(!display.advance(now + SHOW_DELAY + Duration::from_millis(100)));
        assert_eq!(display.next_deadline(), None);
        work.state = ActivityState::Waiting;
        display.update(vec![(id, work)], now + Duration::from_secs(1));
        assert!(display.advance(now + Duration::from_secs(1) + SHOW_DELAY));
        assert_eq!(display.rows(id, false)[0].state, ActivityState::Waiting);
    }

    #[test]
    fn gaps_keep_rows_stable_and_errors_bypass_delay() {
        let now = Instant::now();
        let id = LibraryId::new_v4();
        let mut display = ActivityDisplay::default();
        display.update(vec![(id, summary(ActivityKind::Files, false))], now);
        display.advance(now + SHOW_DELAY);
        display.update(vec![], now + SHOW_DELAY);
        display.advance(now + SHOW_DELAY);
        display.update(vec![(id, summary(ActivityKind::Files, false))], now + SHOW_DELAY + Duration::from_millis(100));
        assert!(!display.advance(now + SHOW_DELAY + Duration::from_millis(100)));
        assert_eq!(display.rows(id, false).len(), 1);
        display.update(vec![(id, summary(ActivityKind::Transfer(TransferJobKind::UploadBook), true))], now + SHOW_DELAY);
        assert!(display.advance(now + SHOW_DELAY));
        assert_eq!(display.rows(id, true)[0].state, ActivityState::Waiting);
        assert!(display.rows(LibraryId::new_v4(), true).is_empty());
    }
}
