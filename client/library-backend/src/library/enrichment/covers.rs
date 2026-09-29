//! Cover acquisition and replacement.
use super::application::ENRICHMENT_WRITE_BATCH_SIZE;
use super::OPEN_LIBRARY_COVER_PROVIDER;
use crate::library::LibrarySession;
use crate::BackendError;
use crate::ContentHash;

impl LibrarySession {
    /// Cover enrichment consumes identifiers already persisted by ISBN enrichment.
    pub(super) async fn enrich_cover_candidates(&self, candidates: Vec<(ContentHash, Vec<String>)>) -> Result<usize, BackendError> {
        let mut updated = 0;
        let mut attempts: Vec<library_database::EnrichmentAttempt> = Vec::new();
        for (hash, isbns) in candidates {
            let expected = self.db.cover_enrichment_state(&hash).map_err(BackendError::operation)?;
            let (state, format) = &expected;
            let Some((state, _)) = state else { continue };
            if format.is_none() {
                continue;
            }
            if state != "no_cover" && state != "ready" {
                continue;
            }
            let outcome: Result<Option<bool>, BackendError> = async {
                let existing = self.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await.map_err(BackendError::operation)?;
                // The selected local or synced cover is authoritative, including
                // square audiobook artwork. Network art fills missing covers.
                if existing.is_some() {
                    return self.apply_enriched_cover(hash, &expected, &existing, None).await;
                }
                for isbn in &isbns {
                    let result = self.metadata().cover(isbn).await;
                    let Some(bytes) = result.map_err(BackendError::operation)? else { continue };
                    let versions = cpu_host::cover(&*self.cpu, bytes).await.map_err(BackendError::operation)?;
                    return self.apply_enriched_cover(hash, &expected, &existing, Some(&versions)).await;
                }
                self.apply_enriched_cover(hash, &expected, &existing, None).await
            }
            .await;
            let (status, detail) = match outcome {
                Ok(Some(true)) => {
                    updated += 1;
                    ("updated", None)
                }
                Ok(Some(false)) => ("no_match", None),
                // Stale lookups must not suppress a future attempt after restore.
                Ok(None) => continue,
                Err(error) => {
                    log::warn!("cover enrichment for {hash} will retry: {error}");
                    ("failed", Some(error.to_string()))
                }
            };
            attempts.push(library_database::EnrichmentAttempt { content_hash: hash, provider: OPEN_LIBRARY_COVER_PROVIDER.to_owned(), identifier: isbns.join(","), status: status.to_owned(), detail });
            if attempts.len() == ENRICHMENT_WRITE_BATCH_SIZE {
                self.flush_cover_outcomes(std::mem::take(&mut attempts)).await?;
            }
        }
        self.flush_cover_outcomes(attempts).await?;
        Ok(updated)
    }

    /// Runs after ISBN artwork, including previously applied matches without ISBNs.
    pub(super) async fn enrich_audible_cover_candidates(&self, candidates: Vec<(ContentHash, String, String)>) -> Result<usize, BackendError> {
        let mut updated = 0;
        for (hash, asin, region) in candidates {
            let identity = (asin.clone(), region.clone());
            if self.db.audible_cover_identity(&hash)? != Some(identity.clone()) {
                continue;
            }
            let expected = self.db.cover_enrichment_state(&hash)?;
            if expected.0.as_ref().map(|s| s.0.as_str()) != Some("no_cover") {
                continue;
            }
            let existing = self.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await.map_err(BackendError::operation)?;
            if existing.is_some() {
                continue;
            }
            let result: Result<Option<bool>, BackendError> = async {
                let bytes = self.metadata().audible_cover(&asin, &region).await.map_err(BackendError::operation)?;
                let versions = match bytes {
                    Some(bytes) => Some(cpu_host::cover(&*self.cpu, bytes).await.map_err(BackendError::operation)?),
                    None => None,
                };
                self.apply_enriched_cover_for_recording(hash, &expected, &existing, versions.as_ref(), Some(&identity)).await
            }
            .await;
            let (status, detail) = match result {
                Ok(Some(true)) => {
                    updated += 1;
                    ("updated", None)
                }
                Ok(Some(false)) => ("no_match", None),
                Ok(None) => continue,
                Err(error) => {
                    log::warn!("Audiobook cover for {hash} will retry: {error}");
                    ("failed", Some(error.to_string()))
                }
            };
            self.db.record_enrichment_attempts(vec![library_database::EnrichmentAttempt { content_hash: hash, provider: "audible_covers_v1".into(), identifier: format!("{region}:{asin}"), status: status.into(), detail }])?;
        }
        Ok(updated)
    }

    /// Attempt records are batched; thumbnail completion is committed while
    /// the short publication lease is still held.
    async fn flush_cover_outcomes(&self, attempts: Vec<library_database::EnrichmentAttempt>) -> Result<(), BackendError> {
        if !attempts.is_empty() {
            self.db.record_enrichment_attempts(attempts).map_err(BackendError::operation)?;
        }
        Ok(())
    }

    /// Network lookup and image processing happen before taking the lease.
    /// Recheck both logical state and stored bytes so a late lookup cannot
    /// overwrite a newer cover or publish for a book moved to trash.
    async fn apply_enriched_cover(&self, hash: ContentHash, expected: &(Option<(String, i64)>, Option<book_model::BookFormat>), existing: &Option<Vec<u8>>, versions: Option<&cpu_host::ThumbnailBytes>) -> Result<Option<bool>, BackendError> {
        self.apply_enriched_cover_for_recording(hash, expected, existing, versions, None).await
    }

    async fn apply_enriched_cover_for_recording(
        &self, hash: ContentHash, expected: &(Option<(String, i64)>, Option<book_model::BookFormat>), existing: &Option<Vec<u8>>, versions: Option<&cpu_host::ThumbnailBytes>, recording: Option<&(String, String)>,
    ) -> Result<Option<bool>, BackendError> {
        let _lease = self.assets.lease_book(&hash).await.map_err(BackendError::operation)?;
        let current = self.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await.map_err(BackendError::operation)?;
        if let Some(identity) = recording {
            if self.db.audible_cover_identity(&hash)?.as_ref() != Some(identity) {
                return Ok(None);
            }
        }
        if &current != existing || self.db.cover_enrichment_state(&hash)? != *expected {
            return Ok(None);
        }
        if let Some(versions) = versions {
            self.store_thumbnail(hash, versions).await?;
        }
        if self.db.cover_enrichment_state(&hash)? != *expected {
            return Ok(None);
        }
        if versions.is_some() || current.is_some() {
            self.db.complete_thumbnail_work(&hash, true).map_err(BackendError::operation)?;
        }
        Ok(Some(versions.is_some()))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn fixture(root: &std::path::Path, origin: &str) -> (LibrarySession, ContentHash) {
        let db = library_database::Database::open(root.join("library.sqlite")).unwrap();
        db.initialize_library().unwrap();
        let hash = book_identity::identify(&mut std::io::Cursor::new(b"cover lease test")).unwrap();
        crate::test_database::raw(&db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'Book','epub')", [hash.as_str()]).unwrap();
        crate::test_database::raw(&db).execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) SELECT ?1,row_id,'book.epub',content_hash,1 FROM book", [sync_common::ROOT_DIR_ID.to_string()]).unwrap();
        db.seed_file_projection(&hash, &sync_common::ROOT_DIR_ID, "/book.epub");
        db.complete_thumbnail_work(&hash, false).unwrap();
        std::fs::write(root.join("book.epub"), b"cover lease test").unwrap();
        let (tx, rx) = crate::library::events::library_event_channel();
        let library = LibrarySession::from_parts(
            crate::LibraryId::new_v4(),
            std::sync::Arc::new(db),
            crate::asset_store::AssetStore::open(root.to_str().unwrap()).unwrap(),
            metadata_client::MetadataClient::new(origin).unwrap().with_cover_origin(origin).unwrap(),
            tx,
            rx,
            crate::library::discovery::LibraryScanner::from_root(root.to_str().unwrap()).unwrap(),
            crate::default_cpu_host(),
        );
        (library, hash)
    }

    fn matched_recording(library: &LibrarySession, hash: ContentHash) {
        crate::test_database::raw(&library.db).execute("UPDATE book SET format='m4b' WHERE content_hash=?1", [hash.as_str()]).unwrap();
        crate::test_database::raw(&library.db).execute("INSERT INTO audiobook_metadata(content_hash,duration_ms) VALUES(?1,600000)", [hash.as_str()]).unwrap();
        let response = serde_json::json!({"status":"supported", "selected_asin":"B000000001", "selected_region":"us", "candidates":[{"asin":"B000000001","region":"us"}]});
        crate::test_database::raw(&library.db)
            .execute(
                "INSERT INTO audible_enrichment_jobs(content_hash,policy,request_json,status,response_json,updated_at) VALUES(?1,'audible_v2','{}','applied',?2,0)",
                library_database::rusqlite::params![hash.as_str(), response.to_string()],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn audible_fallback_uses_selected_recording_without_isbn_and_retains_existing_cover() {
        let root = tempfile::tempdir().unwrap();
        let (library, hash) = fixture(root.path(), "http://127.0.0.1:1");
        matched_recording(&library, hash);
        let scope = serde_json::to_string(&vec![hash]).unwrap();
        let candidates = library.db.audible_cover_candidates("audible_covers_v1", "", &scope).unwrap();
        assert_eq!(candidates, vec![(hash, "B000000001".into(), "us".into())]);
        library.assets.write(crate::BlobKind::Thumbnail, &hash, b"existing cover").await.unwrap();
        assert_eq!(library.enrich_audible_cover_candidates(candidates).await.unwrap(), 0);
        assert_eq!(library.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await.unwrap(), Some(b"existing cover".to_vec()));
        crate::test_database::raw(&library.db).execute("UPDATE audible_enrichment_jobs SET status='ambiguous' WHERE content_hash=?1", [hash.as_str()]).unwrap();
        assert!(library.db.audible_cover_identity(&hash).unwrap().is_none());
        assert!(library.db.audible_cover_candidates("audible_covers_v1", "", &scope).unwrap().is_empty());
    }

    #[test]
    fn mp3_folder_uses_audible_artwork_only_without_a_local_cover() {
        let root = tempfile::tempdir().unwrap();
        let (library, hash) = fixture(root.path(), "http://127.0.0.1:1");
        matched_recording(&library, hash);
        crate::test_database::raw(&library.db).execute("UPDATE book SET format='mp3folder' WHERE content_hash=?1", [hash.as_str()]).unwrap();
        let scope = serde_json::to_string(&vec![hash]).unwrap();
        assert_eq!(library.db.audible_cover_identity(&hash).unwrap(), Some(("B000000001".into(), "us".into())));
        assert_eq!(library.db.audible_cover_candidates("audible_covers_v1", "", &scope).unwrap(), vec![(hash, "B000000001".into(), "us".into())]);

        library.db.complete_sidecar_thumbnail_work(&hash).unwrap();
        assert!(library.db.audible_cover_candidates("audible_covers_v1", "", &scope).unwrap().is_empty());
    }

    #[tokio::test]
    async fn changed_recording_rejects_late_artwork_and_success_completes_thumbnail_work() {
        let root = tempfile::tempdir().unwrap();
        let (library, hash) = fixture(root.path(), "http://127.0.0.1:1");
        matched_recording(&library, hash);
        let expected = library.db.cover_enrichment_state(&hash).unwrap();
        let versions = cpu_host::ThumbnailBytes { browse: vec![1], high_density: vec![2] };
        let wrong = ("B000000002".into(), "us".into());
        assert_eq!(library.apply_enriched_cover_for_recording(hash, &expected, &None, Some(&versions), Some(&wrong)).await.unwrap(), None);
        assert!(library.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await.unwrap().is_none());
        let selected = ("B000000001".into(), "us".into());
        assert_eq!(library.apply_enriched_cover_for_recording(hash, &expected, &None, Some(&versions), Some(&selected)).await.unwrap(), Some(true));
        assert_eq!(library.db.cover_enrichment_state(&hash).unwrap().0.unwrap().0, "ready");
        let scope = serde_json::to_string(&vec![hash]).unwrap();
        assert!(library.db.audible_cover_candidates("audible_covers_v1", "", &scope).unwrap().is_empty());
    }

    #[tokio::test]
    async fn stalled_cover_http_does_not_lease_book_files() {
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let notify = started.clone();
        let app = axum::Router::new().fallback(move || {
            let notify = notify.clone();
            async move {
                notify.notify_one();
                std::future::pending::<axum::http::StatusCode>().await
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let (library, hash) = fixture(root.path(), &origin);
        let mut lookup = Box::pin(library.enrich_cover_candidates(vec![(hash, vec!["9780140328721".into()])]));
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                _ = started.notified() => {},
                result = &mut lookup => panic!("lookup completed before held HTTP: {result:?}"),
            }
            library.db.trash_book(&hash).unwrap();
            library.assets.run_file_jobs(&library.db).await.unwrap();
            assert!(!root.path().join("book.epub").exists(), "trash must acquire its exclusive lease during HTTP");
            assert!(library.db.native_file_work().unwrap().is_empty());
        })
        .await;
        server.abort();
        result.unwrap();
    }

    #[tokio::test]
    async fn unchanged_cover_snapshot_publishes_and_completes_work() {
        let root = tempfile::tempdir().unwrap();
        let (library, hash) = fixture(root.path(), "http://127.0.0.1:1");
        let expected = library.db.cover_enrichment_state(&hash).unwrap();
        let versions = cpu_host::ThumbnailBytes { browse: vec![1], high_density: vec![2] };
        assert_eq!(library.apply_enriched_cover(hash, &expected, &None, Some(&versions)).await.unwrap(), Some(true));
        assert_eq!(library.db.cover_enrichment_state(&hash).unwrap().0.unwrap().0, "ready");
        assert_eq!(library.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await.unwrap(), Some(vec![2]));
    }

    #[tokio::test]
    async fn late_cover_does_not_overwrite_new_bytes_or_revive_a_trashed_book() {
        let root = tempfile::tempdir().unwrap();
        let (library, hash) = fixture(root.path(), "http://127.0.0.1:1");
        let expected = library.db.cover_enrichment_state(&hash).unwrap();
        let versions = cpu_host::ThumbnailBytes { browse: vec![1], high_density: vec![2] };
        library.assets.write(crate::BlobKind::Thumbnail, &hash, b"newer cover").await.unwrap();
        assert!(library.apply_enriched_cover(hash, &expected, &None, Some(&versions)).await.unwrap().is_none());
        assert_eq!(library.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await.unwrap(), Some(b"newer cover".to_vec()));
        library.db.trash_book(&hash).unwrap();
        assert!(library.apply_enriched_cover(hash, &expected, &Some(b"newer cover".to_vec()), Some(&versions)).await.unwrap().is_none());
        assert_eq!(library.db.cover_enrichment_state(&hash).unwrap().0, expected.0);
    }
}
