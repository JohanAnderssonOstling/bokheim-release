//! Owner-committed pages for the storage-independent sync engine.
use crate::engine::PreparedSync;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::{StoreError, StoreFuture};
use library_database::{Database, DatabaseError, OutboxScan};
use sync_common::{PullStateResponse, ReplicaSeq, MAX_PUSH_MUTATIONS};

use library_replica::{LibraryStateEntry, StatePublication};
use library_runtime::events::{LibraryEvent, LibraryEventRouter};
use library_runtime::reading_positions::ReadingPositionPolicy;

/// One scan pins both the durable queue bounds and the owner's policy revision.
pub(crate) struct PublicationOutbox {
    scan: OutboxScan,
    positions: ReadingPositionPolicy,
}

/// Native and browser storage owners prepare the same bounded first page.
pub(crate) fn prepare_sync(database: &Database, events: Option<&LibraryEventRouter>) -> Result<PreparedSync<PublicationOutbox>, DatabaseError> {
    let mut trace = sync_transport::PerformanceTrace::new("sync_prepare", "check_recovery");
    if database.sync_cursor_recovery_pending()? {
        trace.finish(true);
        return Ok(PreparedSync::RecoveryPending);
    }
    trace.phase("read_cursor");
    let cursor = database.sync_pull_cursor()?;
    trace.phase("prepare_outbox");
    let mut outbox = prepare_outbox(database, events)?;
    trace.phase("first_outbox_page");
    let mutations = next_outbox_page(database, events, &mut outbox)?;
    trace.finish(true);
    Ok(PreparedSync::Ready { cursor, outbox, mutations })
}

pub(crate) fn prepare_outbox(database: &Database, events: Option<&LibraryEventRouter>) -> Result<PublicationOutbox, DatabaseError> {
    let positions = events.map(|events| events.reading_positions.borrow().clone()).unwrap_or_default();
    let scan = database.sync_prepare_outbox_excluding_positions(positions.waiting(web_time::Instant::now()))?;
    Ok(PublicationOutbox { scan, positions })
}

pub(crate) fn next_outbox_page(database: &Database, events: Option<&LibraryEventRouter>, outbox: &mut PublicationOutbox) -> Result<Vec<StatePublication>, DatabaseError> {
    let page = database.sync_next_outbox_page(&mut outbox.scan)?;
    if let Some(events) = events {
        let selected = outbox.positions.revisions(page.iter().filter_map(|publication| match &publication.state.value {
            LibraryStateEntry::ReadingPosition { content_hash, .. } => Some(*content_hash),
            _ => None,
        }));
        if !selected.is_empty() {
            events.emit(LibraryEvent::ReadingPositionsSelected(selected));
        }
    }
    Ok(page)
}

/// Storage commands for a complete native exchange. The local
/// library is authoritative, so commands never validate a stored account
/// binding: switching accounts leaves sync state untouched.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct AccountStore<'a> {
    pub sync: &'a super::LibrarySync,
}

pub(crate) fn acknowledge(database: &Database, events: Option<&LibraryEventRouter>, ids: &[ReplicaSeq]) -> Result<(), DatabaseError> {
    for chunk in ids.chunks(MAX_PUSH_MUTATIONS) {
        database.sync_acknowledge_mutations(chunk)?;
    }
    if !ids.is_empty() {
        if let Some(events) = events { events.emit(LibraryEvent::AssetWorkAvailable); }
    }
    Ok(())
}

/// One bounded exchange commits before notifying the owner. No transaction
/// spans an await; independently valid acknowledgements survive a failed pull.
pub(crate) fn apply_response(database: &Database, events: Option<&LibraryEventRouter>, ids: &[ReplicaSeq], page: &PullStateResponse) -> Result<(), DatabaseError> {
    // Normal replies fit the existing protocol limit. Retain bounded handling
    // for callers supplying more IDs than a single exchange could acknowledge.
    if ids.len() > MAX_PUSH_MUTATIONS {
        acknowledge(database, events, ids)?;
        return apply_response(database, events, &[], page);
    }
    match database.sync_commit_pull_response(ids, page) {
        Ok(changed) => {
            if let Some(events) = events {
                if !ids.is_empty() { events.emit(LibraryEvent::AssetWorkAvailable); }
                if changed { events.emit(LibraryEvent::ContentsChanged); }
            }
            Ok(())
        }
        Err(error) if !ids.is_empty() => {
            // The combined command rolled back. Commit only its acknowledgements
            // under a fresh transaction, then report the original pull error.
            acknowledge(database, events, ids)?;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl crate::engine::SyncStore for AccountStore<'_> {
    type Outbox = PublicationOutbox;

    fn prepare_sync(&self) -> StoreFuture<'_, PreparedSync<Self::Outbox>> {
        store_result(prepare_sync(&self.sync.database, self.sync.events.as_ref()))
    }
    fn prepare_outbox(&self) -> StoreFuture<'_, Self::Outbox> {
        store_result(prepare_outbox(&self.sync.database, self.sync.events.as_ref()))
    }
    fn record_mutation_issues(&self, issues: &[sync_common::MutationIssue]) -> StoreFuture<'_, ()> {
        let result = self.sync.database.sync_record_mutation_issues(issues);
        if result.is_ok() {
            if let Some(events) = &self.sync.events {
                events.publish_update(library_model::LibraryUpdate::Contents);
            }
        }
        store_result(result)
    }
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()> {
        store_result(self.sync.database.sync_begin_cursor_recovery())
    }
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize> {
        Box::pin(async { crate::recovery::complete(&self.sync.database).await.map_err(Into::into) })
    }
    fn pending_mutations<'a>(&'a self, scan: &'a mut Self::Outbox) -> StoreFuture<'a, Vec<library_replica::StatePublication>> {
        store_result(next_outbox_page(&self.sync.database, self.sync.events.as_ref(), scan))
    }
    fn acknowledge_mutations(&self, ids: &[ReplicaSeq]) -> StoreFuture<'_, ()> {
        store_result(acknowledge(&self.sync.database, self.sync.events.as_ref(), ids))
    }
    fn apply_exchange_response(&self, ids: &[ReplicaSeq], page: &PullStateResponse) -> StoreFuture<'_, ()> {
        store_result(apply_response(&self.sync.database, self.sync.events.as_ref(), ids, page))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn store_result<T: 'static, E: Into<StoreError> + 'static>(result: Result<T, E>) -> StoreFuture<'static, T> {
    Box::pin(async move { result.map_err(Into::into) })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use library_replica::LibraryStateEntry;
    use sync_common::{Delivery, DirId, LibraryRevision, ReplicaId, SyncCursor, SyncGeneration, ROOT_DIR_ID};

    #[test]
    fn preparation_keeps_the_cursor_and_pages_the_outbox_without_skipping_rows() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("prepare.sqlite3")).unwrap();
        database.initialize_library().unwrap();
        for index in 1..=350 {
            database.create_directory_with_id(&DirId::from_u128(index), &ROOT_DIR_ID, &format!("Shelf {index}")).unwrap();
        }
        let cursor = SyncCursor { generation: Some(SyncGeneration::from_u128(77)), ..Default::default() };
        database.set_sync_pull_cursor(&cursor).unwrap();
        let expected = database.sync_publishable_mutations().unwrap();
        let PreparedSync::Ready { cursor: prepared_cursor, mut outbox, mut mutations } = prepare_sync(&database, None).unwrap() else {
            panic!("no recovery is pending");
        };
        assert_eq!(prepared_cursor, cursor);
        assert!(!mutations.is_empty());
        assert!(mutations.len() < expected.len(), "preparation must retain bounded paging");
        let bytes = rmp_serde::to_vec_named(&PreparedSync::Ready { cursor, outbox: (), mutations: mutations.clone() }).unwrap();
        let decoded: PreparedSync<()> = rmp_serde::from_slice(&bytes).unwrap();
        assert!(matches!(decoded, PreparedSync::Ready { cursor: decoded_cursor, mutations: decoded, .. } if decoded_cursor == cursor && decoded == mutations));
        loop {
            let page = next_outbox_page(&database, None, &mut outbox).unwrap();
            if page.is_empty() { break; }
            mutations.extend(page);
        }
        assert_eq!(mutations, expected);
        assert_eq!(database.sync_pull_cursor().unwrap(), cursor);
        assert_eq!(database.sync_publishable_mutations().unwrap(), expected, "preparation must not acknowledge changes");
    }

    #[test]
    fn pending_recovery_does_not_freeze_or_select_the_outbox() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recover.sqlite3");
        let database = Database::open(&path).unwrap();
        database.initialize_library().unwrap();
        database.create_directory_with_id(&DirId::from_u128(1), &ROOT_DIR_ID, &"Shelf".to_owned()).unwrap();
        database.sync_begin_cursor_recovery().unwrap();
        let probe = rusqlite::Connection::open(&path).unwrap();
        // Normal preparation freezes pending rows with UPDATE before reading them.
        probe.execute_batch("CREATE TRIGGER reject_freeze_before_recovery BEFORE UPDATE ON sync_outbox BEGIN SELECT RAISE(ABORT, 'recovery must finish before outbox preparation'); END;").unwrap();
        assert!(matches!(prepare_sync(&database, None).unwrap(), PreparedSync::RecoveryPending));
        assert!(database.sync_cursor_recovery_pending().unwrap());
    }

    #[test]
    fn an_empty_preparation_roundtrips_through_the_worker_codec() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("empty.sqlite3")).unwrap();
        database.initialize_library().unwrap();
        let PreparedSync::Ready { cursor, mutations, .. } = prepare_sync(&database, None).unwrap() else {
            panic!("no recovery is pending");
        };
        assert!(mutations.is_empty());
        let bytes = rmp_serde::to_vec_named(&PreparedSync::Ready { cursor, outbox: (), mutations }).unwrap();
        let prepared: PreparedSync<()> = rmp_serde::from_slice(&bytes).unwrap();
        assert!(matches!(prepared, PreparedSync::Ready { cursor: decoded, mutations, .. } if decoded == cursor && mutations.is_empty()));
    }

    #[test]
    fn a_full_exchange_commits_acknowledgements_and_cursor_together() {
        let path = std::env::temp_dir().join(format!("combined-exchange-{}.db", ReplicaId::new_v4()));
        let database = Database::open(&path).unwrap();
        database.initialize_library().unwrap();
        for index in 1..=350 {
            database.create_directory_with_id(&DirId::from_u128(index), &ROOT_DIR_ID, &format!("Shelf {index}")).unwrap();
        }
        let pending = database.sync_publishable_mutations().unwrap();
        let ids: Vec<_> = pending.iter().take(MAX_PUSH_MUTATIONS).map(|p| p.publisher_seq).collect();
        assert_eq!(ids.len(), MAX_PUSH_MUTATIONS);
        let generation = SyncGeneration::from_u128(77);
        let probe = rusqlite::Connection::open(&path).unwrap();
        // Each acknowledgement requires the new cursor by commit time. Separate
        // acknowledgement commits would fail this deferred foreign key.
        probe.execute_batch(&format!("
            CREATE UNIQUE INDEX exchange_generation_probe ON sync_metadata(pull_generation);
            CREATE TABLE exchange_ack_probe(generation TEXT REFERENCES sync_metadata(pull_generation) DEFERRABLE INITIALLY DEFERRED);
            CREATE TRIGGER exchange_ack_probe AFTER DELETE ON sync_outbox
            BEGIN INSERT INTO exchange_ack_probe VALUES('{generation}'); END;
        ")).unwrap();
        let page = PullStateResponse { mutations: Vec::new(), next_cursor: SyncCursor { generation: Some(generation), ..Default::default() }, has_more: false };
        apply_response(&database, None, &ids, &page).unwrap();
        assert_eq!(database.sync_pull_cursor().unwrap(), page.next_cursor);
        assert_eq!(database.sync_publishable_mutations().unwrap().len(), pending.len() - ids.len());
        assert_eq!(probe.query_row("SELECT count(*) FROM exchange_ack_probe", [], |row| row.get::<_, i64>(0)).unwrap(), i64::try_from(ids.len()).unwrap());
        drop(probe);
        drop(database);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_pull_rolls_back_its_changes_but_retains_valid_acknowledgements() {
        for acknowledge_sent in [false, true] {
            let path = std::env::temp_dir().join(format!("failed-exchange-{}.db", ReplicaId::new_v4()));
            let database = Database::open(&path).unwrap();
            database.initialize_library().unwrap();
            database.create_directory_with_id(&DirId::from_u128(1), &ROOT_DIR_ID, &"Local".into()).unwrap();
            let pending = database.sync_publishable_mutations().unwrap();
            let ids: Vec<_> = if acknowledge_sent { pending.iter().map(|p| p.publisher_seq).collect() } else { Vec::new() };
            let valid = LibraryStateEntry::DirectoryName { dir_id: DirId::from_u128(2), name: "Remote".into() }.publication(ReplicaId::from_u128(8), 1, ReplicaSeq::new(1).unwrap());
            let invalid_id = DirId::from_u128(3);
            let invalid = LibraryStateEntry::DirectoryName { dir_id: invalid_id, name: "Invalid".into() }.publication(ReplicaId::from_u128(8), 2, ReplicaSeq::new(2).unwrap());
            let probe = rusqlite::Connection::open(&path).unwrap();
            probe.execute_batch(&format!("
                CREATE TRIGGER fail_pull BEFORE INSERT ON sync_state_version
                WHEN NEW.state_kind='directory_name' AND CAST(NEW.state_key AS TEXT)='{invalid_id}'
                BEGIN SELECT RAISE(ABORT, 'injected pull failure'); END;
            ")).unwrap();
            let mutations = [&valid, &invalid].into_iter().enumerate().map(|(index, p)| Delivery {
                state: sync_common::transport::representation(&p.state).unwrap(), revision: LibraryRevision::new(index as u64 + 1).unwrap(),
            }).collect();
            let page = PullStateResponse { mutations, next_cursor: SyncCursor { generation: Some(SyncGeneration::from_u128(77)), ..Default::default() }, has_more: false };
            let before = database.sync_pull_cursor().unwrap();
            let error = apply_response(&database, None, &ids, &page).unwrap_err();
            assert!(error.to_string().contains("injected pull failure"), "{error}");
            assert_eq!(database.sync_pull_cursor().unwrap(), before);
            assert_eq!(database.sync_enqueue_missing_state_cells(&[valid.state.value.address()]).unwrap(), 0, "the earlier remote change must roll back");
            let remaining = database.sync_publishable_mutations().unwrap();
            if acknowledge_sent { assert!(remaining.is_empty()); } else { assert_eq!(remaining, pending); }
            drop(probe);
            drop(database);
            std::fs::remove_file(path).unwrap();
        }
    }
}
