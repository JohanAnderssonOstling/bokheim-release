use serde::Serialize;
use std::time::Instant;
use sync_common::transport::{self, WireError};
use sync_common::{
    ConflictRank, BookId, LibraryId, LibraryRevision, MutationRejection, MutationRejectionReason, OpaquePublication, OpaqueStateEntry, PullStateResponse, PushMutationsResponse, ReplicaId, ReplicaSeq, StateAddress, StateDelivery,
    SyncCursor, SyncExchangeRequest, SyncExchangeResponse, VersionKey, Versioned,
};

// Mirror the old serialization contract without the serde_bytes adapter. This
// checks historical bytes directly, independently of the optimized serializer.
#[derive(Serialize)]
struct LegacyEntry<'a> {
    address: &'a StateAddress,
    book: Option<BookId>,
    bytes: &'a [u8],
}

impl<'a> From<&'a OpaqueStateEntry> for LegacyEntry<'a> {
    fn from(entry: &'a OpaqueStateEntry) -> Self {
        Self { address: &entry.address, book: entry.book, bytes: &entry.bytes }
    }
}

#[derive(Serialize)]
struct LegacyVersion<'a> {
    value: LegacyEntry<'a>,
    version: &'a VersionKey,
}

impl<'a> From<&'a Versioned<OpaqueStateEntry>> for LegacyVersion<'a> {
    fn from(state: &'a Versioned<OpaqueStateEntry>) -> Self {
        Self { value: (&state.value).into(), version: &state.version }
    }
}

#[derive(Serialize)]
struct LegacyDelivery<'a> {
    state: LegacyVersion<'a>,
    revision: LibraryRevision,
}

#[derive(Serialize)]
struct LegacyPull<'a> {
    mutations: Vec<LegacyDelivery<'a>>,
    next_cursor: SyncCursor,
    has_more: bool,
}

#[derive(Serialize)]
struct LegacyResponse<'a> {
    push: &'a PushMutationsResponse,
    pull: LegacyPull<'a>,
}

impl<'a> From<&'a SyncExchangeResponse> for LegacyResponse<'a> {
    fn from(response: &'a SyncExchangeResponse) -> Self {
        Self {
            push: &response.push,
            pull: LegacyPull {
                mutations: response.pull.mutations.iter().map(|delivery| LegacyDelivery { state: (&delivery.state).into(), revision: delivery.revision }).collect(),
                next_cursor: response.pull.next_cursor,
                has_more: response.pull.has_more,
            },
        }
    }
}

fn response(cells: usize, payload_bytes: usize) -> SyncExchangeResponse {
    let replica = ReplicaId::from_u128(1);
    let mutations = (0..cells)
        .map(|index| {
            let sequence = index as u64 + 1;
            let book = BookId::from_bytes([index as u8; 32]);
            let address = if index % 2 == 0 {
                StateAddress { kind: "reading_position".into(), entity_key: book.into(), entity_subkey: "epub".into() }
            } else {
                StateAddress { kind: "directory_name".into(), entity_key: format!("cell-{index:05}").into(), entity_subkey: String::new() }
            };
            StateDelivery {
                state: Versioned {
                    value: OpaqueStateEntry { address, book: Some(book), bytes: (0..payload_bytes).map(|byte| byte as u8).collect() },
                    version: VersionKey::new(1, ConflictRank::Update, replica, ReplicaSeq::new(sequence).unwrap()),
                },
                revision: LibraryRevision::new(sequence).unwrap(),
            }
        })
        .collect();
    SyncExchangeResponse {
        push: PushMutationsResponse { rejected: vec![MutationRejection { publisher_seq: ReplicaSeq::new(1).unwrap(), reason: MutationRejectionReason::TimestampTooFarFuture }] },
        pull: PullStateResponse { mutations, next_cursor: SyncCursor { generation: Some(LibraryId::from_u128(2)), field_revision: LibraryRevision::new(cells as u64).ok(), ..SyncCursor::default() }, has_more: false },
    }
}

fn assert_response(decoded: SyncExchangeResponse, expected: &SyncExchangeResponse) {
    assert_eq!(decoded.push.rejected, expected.push.rejected);
    assert_eq!(decoded.pull.mutations, expected.pull.mutations);
    assert_eq!(decoded.pull.next_cursor, expected.pull.next_cursor);
    assert_eq!(decoded.pull.has_more, expected.pull.has_more);
}

#[test]
fn sync_response_compacts_bookkeeping_and_reads_historical_payloads() {
    for payload in [0, 256, 128 * 1024] {
        let expected = response(4, payload);
        let legacy = transport::encode(&LegacyResponse::from(&expected)).unwrap();
        let compact = transport::encode(&expected).unwrap();
        assert_eq!(transport::encoded_len(&expected).unwrap(), compact.len());
        assert!(compact.len() < legacy.len());
        assert_response(transport::decode(&compact, transport::MAX_DECODED_RESPONSE_BYTES).unwrap(), &expected);
        assert_response(transport::decode_byte_fields(&legacy, transport::MAX_DECODED_RESPONSE_BYTES).unwrap(), &expected);
        let mut historical = legacy;
        historical[1] = 30;
        assert_response(transport::decode_state(&historical, transport::MAX_DECODED_RESPONSE_BYTES).unwrap(), &expected);
    }
}

#[test]
fn opaque_entries_keep_persisted_storage_and_json_layouts() {
    let expected = response(4, 256);
    for delivery in expected.pull.mutations {
        let entry = delivery.state.value;
        let legacy = LegacyEntry::from(&entry);
        let persisted = sync_common::wire::encode(&legacy).unwrap();
        assert_eq!(sync_common::wire::encode(&entry).unwrap(), persisted);
        assert_eq!(sync_common::wire::decode::<OpaqueStateEntry>(&persisted, 4096).unwrap(), entry);
        assert_eq!(serde_json::to_value(&entry).unwrap(), serde_json::to_value(&legacy).unwrap());
    }
}

#[test]
fn exchange_request_compact_reader_preserves_publication_metadata() {
    let expected = response(4, 256);
    let request = SyncExchangeRequest {
        library_id: LibraryId::from_u128(3),
        replica_id: ReplicaId::from_u128(1),
        cursor: expected.pull.next_cursor,
        mutations: expected.pull.mutations.into_iter().map(|delivery| OpaquePublication { publisher_seq: delivery.state.version.replica_seq, state: delivery.state }).collect(),
    };
    let bytes = transport::encode(&request).unwrap();
    assert_eq!(transport::encoded_len(&request).unwrap(), bytes.len());
    let decoded: SyncExchangeRequest = transport::decode_byte_fields(&bytes, transport::MAX_DECODED_REQUEST_BYTES).unwrap();
    assert_eq!(decoded.library_id, request.library_id);
    assert_eq!(decoded.replica_id, request.replica_id);
    assert_eq!(decoded.cursor, request.cursor);
    assert_eq!(decoded.mutations, request.mutations);
}

#[test]
fn compact_exchange_reads_empty_pull_push_and_request_lists() {
    let expected = SyncExchangeResponse { push: PushMutationsResponse { rejected: Vec::new() }, pull: PullStateResponse { mutations: Vec::new(), next_cursor: SyncCursor::default(), has_more: false } };
    let bytes = transport::encode(&expected).unwrap();
    assert_response(transport::decode_byte_fields(&bytes, transport::MAX_DECODED_RESPONSE_BYTES).unwrap(), &expected);
    let request = SyncExchangeRequest { library_id: LibraryId::from_u128(3), replica_id: ReplicaId::from_u128(1), cursor: SyncCursor::default(), mutations: Vec::new() };
    let decoded: SyncExchangeRequest = transport::decode_byte_fields(&transport::encode(&request).unwrap(), transport::MAX_DECODED_REQUEST_BYTES).unwrap();
    assert!(decoded.mutations.is_empty());
}

#[test]
fn counted_push_sizes_keep_exact_batch_limits_and_oversized_handling() {
    use sync_common::{PushBatcher, MAX_PUSH_BATCH_BYTES};
    let make_publication = |sequence, payload| {
        let state = response(1, payload).pull.mutations.remove(0).state;
        OpaquePublication { publisher_seq: ReplicaSeq::new(sequence).unwrap(), state }
    };
    let mut exact = make_publication(1, MAX_PUSH_BATCH_BYTES);
    let extra = transport::encoded_len(&exact).unwrap() - MAX_PUSH_BATCH_BYTES;
    exact.state.value.bytes.truncate(MAX_PUSH_BATCH_BYTES - extra);
    assert_eq!(transport::encoded_len(&exact).unwrap(), MAX_PUSH_BATCH_BYTES);
    assert_eq!(transport::encode(&exact).unwrap().len(), MAX_PUSH_BATCH_BYTES);
    let mut oversized = exact.clone();
    oversized.publisher_seq = ReplicaSeq::new(2).unwrap();
    oversized.state.value.bytes.push(1);
    assert_eq!(transport::encoded_len(&oversized).unwrap(), MAX_PUSH_BATCH_BYTES + 1);
    let small = make_publication(3, 128);
    let mut batches = PushBatcher::new(vec![exact.clone(), oversized, small.clone()]).unwrap();
    assert_eq!(batches.untransmittable(), &[ReplicaSeq::new(2).unwrap()]);
    assert_eq!(batches.next_batch(), vec![exact]);
    assert_eq!(batches.next_batch(), vec![small]);
    assert!(batches.is_empty());
    assert!(!batches.has_batch());
}

#[test]
fn incremental_batch_budget_recovers_after_deferrals_and_oversized_values() {
    use sync_common::{PushBatcher, MAX_PUSH_BATCH_BYTES};
    let state = response(1, MAX_PUSH_BATCH_BYTES).pull.mutations.remove(0).state;
    let mut exact = OpaquePublication { publisher_seq: ReplicaSeq::new(1).unwrap(), state };
    let overhead = transport::encoded_len(&exact).unwrap() - MAX_PUSH_BATCH_BYTES;
    exact.state.value.bytes.truncate(MAX_PUSH_BATCH_BYTES - overhead);
    let book = exact.state.value.book.unwrap();
    let mut batcher = PushBatcher::default();
    batcher.push(exact.clone()).unwrap();
    assert!(batcher.has_batch());
    batcher.defer_book_fields(&[book]);
    assert!(!batcher.has_batch());
    assert!(batcher.is_empty());
    let mut oversized = exact.clone();
    oversized.state.value.bytes.push(1);
    batcher.push(oversized).unwrap();
    assert_eq!(batcher.take_untransmittable(), vec![exact.publisher_seq]);
    assert!(batcher.take_untransmittable().is_empty());
    batcher.push(exact.clone()).unwrap();
    assert_eq!(batcher.next_batch(), vec![exact]);
    assert!(!batcher.has_batch());
}

#[test]
fn incremental_batching_preserves_eager_batch_boundaries() {
    use sync_common::PushBatcher;
    let publications: Vec<_> = response(40, 128 * 1024).pull.mutations.into_iter().map(|delivery| OpaquePublication {
        publisher_seq: delivery.state.version.replica_seq, state: delivery.state,
    }).collect();
    let mut eager = PushBatcher::new(publications.clone()).unwrap();
    let mut expected = Vec::new();
    while !eager.is_empty() { expected.push(eager.next_batch()); }
    let mut incremental = PushBatcher::default();
    let mut actual = Vec::new();
    for publication in publications {
        incremental.push(publication).unwrap();
        if incremental.has_batch() { actual.push(incremental.next_batch()); }
    }
    while !incremental.is_empty() { actual.push(incremental.next_batch()); }
    assert_eq!(actual, expected);
}

#[test]
fn deferred_books_preserve_other_publications_and_iterator_errors_stop_preparation() {
    let publications: Vec<_> = response(40, 16).pull.mutations.into_iter().map(|delivery| OpaquePublication { publisher_seq: delivery.state.version.replica_seq, state: delivery.state }).collect();
    for count in [0, 1, 15, 16, 32] {
        let books: Vec<_> = (0..count).map(|index| BookId::from_bytes([index as u8; 32])).collect();
        let expected: Vec<_> = publications.iter().filter(|publication| !publication.state.value.book.is_some_and(|book| books.contains(&book))).cloned().collect();
        let mut batcher = sync_common::PushBatcher::try_from_publications(publications.clone().into_iter().map(Ok)).unwrap();
        batcher.defer_book_fields(&books);
        assert_eq!(batcher.next_batch(), expected);
        assert!(batcher.is_empty());
    }
    let mut consumed = 0;
    let stream = publications.into_iter().enumerate().map(|(index, publication)| {
        consumed += 1;
        if index == 1 { Err(WireError::InvalidHeader) } else { Ok(publication) }
    });
    assert!(matches!(sync_common::PushBatcher::try_from_publications(stream), Err(WireError::InvalidHeader)));
    assert_eq!(consumed, 2);
}

/// Compare identical pages in separate processes so peak memory is meaningful.
/// SYNC_CODEC_BENCH_MODE selects legacy or compact; ten warmed samples per case.
#[test]
#[ignore = "release benchmark; set SYNC_CODEC_BENCH_MODE=legacy or compact"]
fn sync_byte_codec_performance() {
    let mode = std::env::var("SYNC_CODEC_BENCH_MODE").unwrap();
    assert!(mode == "legacy" || mode == "compact");
    for (name, cells, payload) in [("small", 1000, 128), ("large", 96, 128 * 1024)] {
        let response = response(cells, payload);
        let legacy = LegacyResponse::from(&response);
        let encode = || if mode == "legacy" { transport::encode(&legacy) } else { transport::encode(&response) };
        let decode = |bytes: &[u8]| -> Result<SyncExchangeResponse, WireError> {
            if mode == "legacy" {
                transport::decode(bytes, transport::MAX_DECODED_RESPONSE_BYTES)
            } else {
                transport::decode_byte_fields(bytes, transport::MAX_DECODED_RESPONSE_BYTES)
            }
        };
        assert_response(decode(&encode().unwrap()).unwrap(), &response);
        let mut encodes = Vec::new();
        let mut decodes = Vec::new();
        let mut body_bytes = 0;
        for _ in 0..10 {
            let started = Instant::now();
            let bytes = encode().unwrap();
            encodes.push(started.elapsed().as_secs_f64() * 1000.0);
            body_bytes = bytes.len();
            let started = Instant::now();
            let received = decode(&bytes).unwrap();
            decodes.push(started.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(received.pull.mutations.len(), cells);
        }
        encodes.sort_by(f64::total_cmp);
        decodes.sort_by(f64::total_cmp);
        println!("CODEC mode={mode} case={name} cells={cells} body_bytes={body_bytes} median_encode_ms={:.3} median_decode_ms={:.3}", encodes[5], decodes[5]);
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find(|line| line.starts_with("VmHWM:")) {
            println!("CODEC {line}");
        }
    }
}

#[test]
#[ignore = "release benchmark of push batch sizing"]
fn sync_push_batch_performance() {
    for (name, cells, payload) in [("small", 1000, 128), ("large", 30, 128 * 1024)] {
        let publications: Vec<_> = response(cells, payload).pull.mutations.into_iter().map(|delivery| OpaquePublication { publisher_seq: delivery.state.version.replica_seq, state: delivery.state }).collect();
        let mut samples = Vec::new();
        for _ in 0..11 {
            let input = publications.clone();
            let started = Instant::now();
            let mut batcher = sync_common::PushBatcher::new(input).unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            assert!(batcher.untransmittable().is_empty());
            let mut count = 0;
            while !batcher.is_empty() {
                count += batcher.next_batch().len();
            }
            assert_eq!(count, cells);
        }
        samples.remove(0);
        samples.sort_by(f64::total_cmp);
        println!("SIZING case={name} cells={cells} median_ms={:.3}", samples[5]);
    }
}

#[test]
#[ignore = "release benchmark of successful and rejected outbox batches"]
fn sync_outbox_queue_performance() {
    for cells in [1_000, 10_000, 100_000] {
        let publications: Vec<_> = response(cells, 128).pull.mutations.into_iter().map(|delivery| OpaquePublication { publisher_seq: delivery.state.version.replica_seq, state: delivery.state }).collect();
        for failed_books in [0, 128] {
            let books: Vec<_> = (0..failed_books).map(|index| BookId::from_bytes([index as u8; 32])).collect();
            let mut samples = Vec::new();
            let mut drained = 0;
            for _ in 0..7 {
                let mut batcher = sync_common::PushBatcher::new(publications.clone()).unwrap();
                drained = 0;
                let started = Instant::now();
                batcher.defer_book_fields(&books);
                while !batcher.is_empty() {
                    drained += batcher.next_batch().len();
                    batcher.defer_book_fields(&[]);
                }
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples.remove(0);
            samples.sort_by(f64::total_cmp);
            println!("QUEUE cells={cells} failed_books={failed_books} drained={drained} median_ms={:.3}", samples[3]);
        }
    }
}
