//! Contributor identity lookups and writes, shared by import and projection.
use crate::SqlParam;
pub(crate) use crate::shared_sql::SharedSql;
pub(crate) use crate::{DatabaseError, import::ImportCommitSql, sync::SyncApplySql};
pub(crate) use book_model::{AUTHOR_MARC_RELATOR_CODE, AgentId, Contributor, normalize_search_text};
use content_address::BookId;
use metadata_contract::matching::author_match_keys;
use std::collections::BTreeSet;

pub(crate) type Identity = (i64, AgentId);
pub(crate) type StoredCredit = (i64, Contributor);

/// Normalized names use the persisted key index. Callers supplying a different
/// equivalence rule retain an explicit pairwise matching option.
#[derive(Clone, Copy)]
pub enum ContributorNameMatcher {
    Normalized,
    Custom(fn(&str, &str) -> bool),
}

fn write_name_keys(tx: &rusqlite::Transaction<'_>, local: i64, keys: &BTreeSet<String>) -> Result<(), DatabaseError> {
    for key in keys {
        tx.contributors_insert_name_key(key, local)?;
    }
    Ok(())
}

/// Populate the local index once when upgrading an existing library. Matching
/// rule changes must rebuild this index in a schema migration.
pub(crate) fn backfill_name_keys(tx: &rusqlite::Transaction<'_>) -> Result<(), DatabaseError> {
    tx.contributors_existing_names(|row| {
        let local = row.get(0)?;
        let name: String = row.get(1)?;
        for key in author_match_keys(&name) {
            tx.contributors_insert_name_key(&key, local)?;
        }
        Ok(())
    })?;
    Ok(())
}

pub(crate) fn resolve_identity(tx: &rusqlite::Transaction<'_>, id: AgentId, name: &str, matcher: Option<ContributorNameMatcher>) -> Result<Identity, DatabaseError> {
    let mut existing: Option<(i64, String)> = None;
    tx.author_identity_by_stable_id(&id.to_string(), |row| {
        existing = Some((row.get(0)?, row.get(1)?));
        Ok(())
    })?;
    // Stable IDs take precedence even when another identity has the same name.
    let keys = if existing.is_none() { author_match_keys(name) } else { BTreeSet::new() };
    if existing.is_none() {
        match matcher {
            Some(ContributorNameMatcher::Normalized) => {
                for key in &keys {
                    tx.contributors_identity_by_name_key(key, |row| {
                        let local = row.get(0)?;
                        // Preserve the first identity in insertion order across
                        // all matching keys, just as the former ordered scan did.
                        if existing.as_ref().is_none_or(|(found, _)| local < *found) {
                            existing = Some((local, row.get(1)?));
                        }
                        Ok(())
                    })?;
                }
            }
            Some(ContributorNameMatcher::Custom(matches)) => {
                tx.provisional_author_identities(|row| {
                    if existing.is_none() && matches(name, &row.get::<_, String>(2)?) {
                        existing = Some((row.get(0)?, row.get(1)?));
                    }
                    Ok(())
                })?;
            }
            None => {}
        }
    }
    match existing {
        Some((local, stable)) => Ok((local, AgentId::parse_str(&stable).map_err(|e| DatabaseError::message(format!("stored author identity is not readable: {e}")))?)),
        None => {
            tx.shared_authors_insert_author_identity(&id.to_string(), name)?;
            let local = tx.last_insert_rowid();
            write_name_keys(tx, local, &keys)?;
            Ok((local, id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_methods::query_rows;
    use crate::{Database, transactions::WriteOutcome};

    #[test]
    fn indexed_names_reuse_the_first_identity_and_keep_credited_names() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.write(|tx| {
            let first = Contributor::new("García, José", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let second = Contributor::new("Jose Garcia", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let identities = resolve_credits(tx, &[first.clone(), second.clone()], None)?;
            let variant = Contributor::new("Dr Jose Garcia", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let matched = resolve_credits(tx, &[variant], Some(ContributorNameMatcher::Normalized))?;
            assert_eq!(matched[0].0, identities[0].0);
            assert_eq!(matched[0].1.agent_id(), first.agent_id());
            assert_eq!(matched[0].1.name(), "Dr Jose Garcia");
            // An exact stable ID wins over an earlier matching name.
            let exact = resolve_credits(tx, &[second], Some(ContributorNameMatcher::Normalized))?;
            assert_eq!(exact[0].0, identities[1].0);
            let stored_name: String = tx.query_row("SELECT name FROM author_identity WHERE id=?1", [identities[0].0], |row| row.get(0))?;
            assert_eq!(stored_name, "García, José");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn indexed_names_include_new_identities_but_exclude_confirmed_ones_and_empty_keys() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.write(|tx| {
            let names = ["John Smith", "Smith, John", "...", "---"];
            let credits = names.map(|name| Contributor::new(name, AUTHOR_MARC_RELATOR_CODE).unwrap());
            let stored = resolve_credits(tx, &credits, Some(ContributorNameMatcher::Normalized))?;
            assert_eq!(stored[0].0, stored[1].0);
            assert_ne!(stored[2].0, stored[3].0);
            tx.execute("UPDATE author_identity SET is_provisional=0 WHERE id=?1", [stored[0].0])?;
            let credit = Contributor::new("Dr John Smith", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let distinct = resolve_credits(tx, &[credit], Some(ContributorNameMatcher::Normalized))?;
            assert_ne!(distinct[0].0, stored[0].0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn name_index_migration_backfills_existing_identities() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let original = Contributor::new("García, José", AUTHOR_MARC_RELATOR_CODE).unwrap();
        db.write(|tx| {
            resolve_credits(tx, &[original.clone()], None)?;
            Ok(())
        })
        .unwrap();
        // A version-43 database has identities but no persisted name keys.
        db.connection.execute_batch("DROP TABLE author_name_key; PRAGMA user_version=43;").unwrap();
        db.initialize_library().unwrap();
        db.write(|tx| {
            let other = Contributor::new("Jose Garcia", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let stored = resolve_credits(tx, &[other], Some(ContributorNameMatcher::Normalized))?;
            assert_eq!(stored[0].1.agent_id(), original.agent_id());
            Ok(())
        })
        .unwrap();
        assert_eq!(db.connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)).unwrap(), crate::LIBRARY_SCHEMA_VERSION);
    }

    #[test]
    fn name_keys_rollback_with_identity_creation() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.with_write_transaction(|tx| {
            let credit = Contributor::new("Rollback Author", AUTHOR_MARC_RELATOR_CODE).unwrap();
            resolve_credits(tx, &[credit], Some(ContributorNameMatcher::Normalized))?;
            Ok(WriteOutcome::Rollback(()))
        })
        .unwrap();
        let count: i64 = db.connection.query_row("SELECT count(*) FROM author_name_key", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn normalized_lookup_uses_the_key_index_without_scanning_identities() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.write(|tx| {
            for index in 0..256 {
                let credit = Contributor::new(&format!("Unrelated Author {index}"), AUTHOR_MARC_RELATOR_CODE).unwrap();
                resolve_credits(tx, &[credit], None)?;
            }
            let credit = Contributor::new("García, José", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let stored = resolve_credits(tx, &[credit], None)?;
            let mut found = false;
            for key in author_match_keys("Jose Garcia") {
                tx.contributors_identity_by_name_key(&key, |row| {
                    assert_eq!(row.get::<_, i64>(0)?, stored[0].0);
                    assert_eq!(crate::query_methods::active_query_fullscan_steps(tx), 0);
                    found = true;
                    Ok(())
                })?;
            }
            assert!(found);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn credit_resolution_uses_only_the_supplied_matching_rule() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.write(|tx| {
            let first = Contributor::new("Same Name", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let other = Contributor::new("Same Name", AUTHOR_MARC_RELATOR_CODE).unwrap();
            let original = resolve_credits(tx, &[first], None)?;
            let reused = resolve_credits(tx, &[other.clone()], Some(ContributorNameMatcher::Custom(|left, right| left == right)))?;
            let independent = resolve_credits(tx, &[other], None)?;
            assert_eq!(original[0].1.agent_id(), reused[0].1.agent_id());
            assert_ne!(original[0].1.agent_id(), independent[0].1.agent_id());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cached_credit_writes_rebind_books_roles_and_empty_lists() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let first = BookId::new(&"a".repeat(64));
        let second = BookId::new(&"b".repeat(64));
        let author = Contributor::new("First Author", book_model::MarcRelatorCode(*b"aut")).unwrap();
        let editor = Contributor::new("Second Author", book_model::MarcRelatorCode(*b"edt")).unwrap();
        db.with_write_transaction(|tx| {
            for hash in [&first, &second] {
                tx.fixtures_insert_book(&hash)?;
            }
            let credits = resolve_credits(tx, &[author, editor], None)?;
            write_credits(tx, &first, &credits)?;
            write_credits(tx, &second, &credits[..1])?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        db.with_write_transaction(|tx| {
            write_credits(tx, &first, &[])?;
            let replacement = Contributor::new("Replacement", book_model::MarcRelatorCode(*b"trl")).unwrap();
            let credits = resolve_credits(tx, &[replacement], None)?;
            write_credits(tx, &second, &credits)?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        let rows = query_rows(|callback| db.connection.fixtures_read_book_contributor_b_content_hash_c(callback), |row| Ok((row.get::<_, BookId>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![(second, 0, "Replacement".to_owned(), b"trl".to_vec())]);
    }
}

pub(crate) fn resolve_credits(tx: &rusqlite::Transaction<'_>, credits: &[Contributor], matcher: Option<ContributorNameMatcher>) -> Result<Vec<StoredCredit>, DatabaseError> {
    let mut stored = Vec::new();
    for credit in credits {
        let identity = resolve_identity(tx, credit.agent_id(), credit.name(), matcher)?;
        let credit = Contributor::with_id(identity.1, credit.name(), credit.role_code()).map_err(|e| DatabaseError::message(format!("contributor credit is not writable: {e}")))?;
        stored.push((identity.0, credit));
    }
    Ok(stored)
}

pub(crate) fn write_credits(tx: &rusqlite::Transaction<'_>, hash: &BookId, credits: &[StoredCredit]) -> Result<(), DatabaseError> {
    if !credits.is_empty() {
        for (position, (local, credit)) in credits.iter().enumerate() {
            tx.contributors_upsert_credit(&hash, &local, &(i64::try_from(position).unwrap_or(i64::MAX)), &(credit.name()), &(&credit.role_code().0))?;
        }
    }
    tx.contributors_trim_credits(&hash, &(i64::try_from(credits.len()).unwrap_or(i64::MAX)))?;
    let author = credits.iter().filter(|(_, credit)| credit.role_code() == AUTHOR_MARC_RELATOR_CODE).map(|(_, credit)| credit.name()).collect::<Vec<_>>().join(", ");
    tx.contributors_set_author(&hash, &author, &(normalize_search_text(&author)))?;
    Ok(())
}

include_sqlite_sql::include_sql!("src/sql/contributors.sql");
#[cfg(test)]
use crate::sql_fixtures::FixturesSql;
