//! Shared identity primitives; import may reuse provisional names, sync must not.
use crate::shared_sql::SharedSql;
use crate::sync::{parse_author_name_opt, unix_millis};
use crate::{DatabaseError, import::ImportCommitSql, sync::SyncApplySql};
use book_model::{AgentId, AuthorName, Contributor, PublisherCredit};

type Identity = (i64, AgentId);
pub(crate) type StoredCredit = (i64, Contributor);

#[derive(Clone, Copy)]
pub(crate) enum IdentityPolicy {
    StableOnly,
    ReuseProvisional,
}

pub(crate) fn resolve_identity(tx: &rusqlite::Transaction<'_>, id: AgentId, name: &AuthorName, policy: IdentityPolicy) -> Result<Identity, DatabaseError> {
    let mut existing: Option<(i64, String)> = None;
    tx.author_identity_by_stable_id(&id.to_string(), |row| {
        existing = Some((row.get(0)?, row.get(1)?));
        Ok(())
    })?;
    if existing.is_none() && matches!(policy, IdentityPolicy::ReuseProvisional) {
        tx.provisional_author_identity_by_name(name.match_key().as_str(), |row| {
            existing = Some((row.get(0)?, row.get(1)?));
            Ok(())
        })?;
    }
    match existing {
        Some((local, stable)) => Ok((local, AgentId::parse_str(&stable).map_err(|e| DatabaseError::message(format!("stored author identity is not readable: {e}")))?)),
        None => {
            tx.shared_authors_insert_author_identity(&id.to_string(), name.as_str(), name.match_key().as_str(), unix_millis()?)?;
            Ok((tx.last_insert_rowid(), id))
        }
    }
}

fn reidentify(identity: Identity, credit: &Contributor) -> Result<StoredCredit, DatabaseError> {
    let credit = Contributor::with_id(identity.1, credit.name(), credit.role_code()).map_err(|e| DatabaseError::message(format!("contributor credit is not writable: {e}")))?;
    Ok((identity.0, credit))
}

pub(crate) fn resolve_credits(tx: &rusqlite::Transaction<'_>, credits: &[Contributor]) -> Result<Vec<StoredCredit>, DatabaseError> {
    let mut stored = Vec::new();
    for credit in credits {
        let Some(name) = parse_author_name_opt(credit.name())? else { continue };
        stored.push(reidentify(resolve_identity(tx, credit.agent_id(), &name, IdentityPolicy::StableOnly)?, credit)?);
    }
    Ok(stored)
}

/// Reimport first preserves the existing credit at this position when its name
/// still matches, then falls back to stable ID and provisional-name resolution.
pub(crate) fn resolve_import_credits(tx: &rusqlite::Transaction<'_>, hash: &str, credits: &[Contributor]) -> Result<Vec<StoredCredit>, DatabaseError> {
    let mut stored = Vec::new();
    for (position, credit) in credits.iter().enumerate() {
        let Some(name) = parse_author_name_opt(credit.name())? else { continue };
        let mut existing: Option<(i64, String, String)> = None;
        tx.existing_book_contributor_credit(hash, i64::try_from(position).unwrap_or(i64::MAX), |row| {
            existing = Some((row.get(0)?, row.get(1)?, row.get(2)?));
            Ok(())
        })?;
        let mut reused = None;
        if let Some((local, stable, old_name)) = existing {
            if let Ok(Some(old_name)) = parse_author_name_opt(&old_name) {
                if old_name.match_key() == name.match_key() {
                    reused = Some((local, AgentId::parse_str(&stable).map_err(|e| DatabaseError::message(format!("stored author identity is not readable: {e}")))?));
                }
            }
        }
        let identity = match reused {
            Some(identity) => identity,
            None => resolve_identity(tx, credit.agent_id(), &name, IdentityPolicy::ReuseProvisional)?,
        };
        stored.push(reidentify(identity, credit)?);
    }
    Ok(stored)
}

pub(crate) fn resolve_publishers(tx: &rusqlite::Transaction<'_>, publishers: &[PublisherCredit], policy: IdentityPolicy) -> Result<Vec<PublisherCredit>, DatabaseError> {
    publishers
        .iter()
        .map(|publisher| {
            let name = parse_author_name_opt(publisher.name().as_str())?.ok_or_else(|| DatabaseError::message("publisher name did not produce an agent name"))?;
            let (_, stable) = resolve_identity(tx, publisher.agent_id(), &name, policy)?;
            PublisherCredit::with_id(stable, publisher.name().as_str()).map_err(|e| DatabaseError::message(format!("publisher credit is not writable: {e}")))
        })
        .collect()
}

/// Caller controls remote origin or local metadata batching around these writes.
pub(crate) fn write_credits(tx: &rusqlite::Transaction<'_>, hash: &str, credits: &[StoredCredit]) -> Result<(), DatabaseError> {
    let mut timing = crate::timing::PhaseTimer::new("write_credits");
    if !credits.is_empty() {
        let mut statement = tx.prepare_cached(include_str!("sql/upsert_contributor.sql"))?;
        for (position, (local, credit)) in credits.iter().enumerate() {
            statement.execute(rusqlite::named_params! {
                ":content_hash": hash, ":author_identity_id": local,
                ":position": i64::try_from(position).unwrap_or(i64::MAX),
                ":name": credit.name(), ":role": &credit.role_code().0,
            })?;
        }
    }
    timing.mark("upsert_contributors");
    tx.prepare_cached(include_str!("sql/trim_contributors.sql"))?.execute(rusqlite::named_params! {
        ":content_hash": hash, ":position": i64::try_from(credits.len()).unwrap_or(i64::MAX),
    })?;
    timing.mark("trim_contributors");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, transactions::WriteOutcome};

    #[test]
    fn cached_credit_writes_rebind_books_roles_and_empty_lists() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let first = "a".repeat(64);
        let second = "b".repeat(64);
        let author = Contributor::new("First Author", book_model::MarcRelatorCode(*b"aut")).unwrap();
        let editor = Contributor::new("Second Author", book_model::MarcRelatorCode(*b"edt")).unwrap();
        db.with_write_transaction(|tx| {
            for hash in [&first, &second] {
                tx.execute("INSERT INTO book(content_hash,title,added_at,format) VALUES (?1,'Book',1,'epub')", [hash])?;
            }
            let credits = resolve_credits(tx, &[author, editor])?;
            write_credits(tx, &first, &credits)?;
            write_credits(tx, &second, &credits[..1])?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        db.with_write_transaction(|tx| {
            write_credits(tx, &first, &[])?;
            let replacement = Contributor::new("Replacement", book_model::MarcRelatorCode(*b"trl")).unwrap();
            let credits = resolve_credits(tx, &[replacement])?;
            write_credits(tx, &second, &credits)?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        let rows = db.connection
            .prepare("SELECT b.content_hash,c.position,c.name,c.role FROM book_contributor c JOIN book b ON b.row_id=c.book_row_id ORDER BY b.content_hash,c.position")
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![(second, 0, "Replacement".to_owned(), b"trl".to_vec())]);
    }

    #[test]
    fn import_reuses_names_but_sync_preserves_distinct_stable_identities() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.with_write_transaction(|tx| {
            let name = AuthorName::parse("Same Name").unwrap();
            let first = AgentId::new_v4();
            let second = AgentId::new_v4();
            let original = resolve_identity(tx, first, &name, IdentityPolicy::StableOnly)?;
            assert_eq!(resolve_identity(tx, second, &name, IdentityPolicy::ReuseProvisional)?, original);
            let separate = resolve_identity(tx, second, &name, IdentityPolicy::StableOnly)?;
            assert_ne!(separate.0, original.0);
            assert_eq!(separate.1, second);
            // Stable IDs take precedence even when another provisional name matches.
            assert_eq!(resolve_identity(tx, second, &name, IdentityPolicy::ReuseProvisional)?, separate);
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
    }

    #[test]
    fn reimport_preserves_credited_identity_before_falling_back_to_incoming_id() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.with_write_transaction(|tx| {
            let hash = "b".repeat(64);
            tx.execute("INSERT INTO book(content_hash,title,added_at,format) VALUES (?1,'Book',1,'epub')", [&hash])?;
            let first = Contributor::new("An Author", book_model::MarcRelatorCode(*b"aut")).unwrap();
            let stored = resolve_credits(tx, &[first.clone()])?;
            write_credits(tx, &hash, &stored)?;
            let incoming = Contributor::new("An Author", book_model::MarcRelatorCode(*b"aut")).unwrap();
            let separate = resolve_credits(tx, &[incoming.clone()])?;
            assert_ne!(separate[0].0, stored[0].0);
            let imported = resolve_import_credits(tx, &hash, &[incoming])?;
            assert_eq!(imported[0], stored[0]);
            write_credits(tx, &hash, &[])?;
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM book_contributor", [], |r| r.get::<_, i64>(0))?, 0);
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
    }
}
