//! Conservative cross-work LCC recovery. Never changes the requested edition's identity.
use crate::import::core::author_match_keys;
use crate::{error, open_library_id, Classification, ClassificationScheme, ClassificationSource, Connection, IsbnClassificationResult, MetadataError};
use metadata_contract::matching::bibliographic_title_match_key as bibliographic_match_key;
use rusqlite::OptionalExtension;
use std::collections::{BTreeMap, BTreeSet};

const CANDIDATE_LIMIT: usize = 128;

fn title_key(title: &str) -> Option<String> {
    let normalized = bibliographic_match_key(title);
    // These editions need explicit identity evidence beyond a title/author match.
    if normalized
        .split_whitespace()
        .any(|w| matches!(w, "abridged" | "abridgment" | "abridgement" | "adapted" | "adaptation" | "retold" | "retelling" | "study" | "guide" | "workbook" | "summary" | "summaries" | "selections" | "extracts" | "volume" | "vol" | "part"))
    {
        return None;
    }
    let primary = title.split(':').next().unwrap_or(title);
    let key = bibliographic_match_key(primary);
    let key = ["the ", "an ", "a "].iter().find_map(|article| key.strip_prefix(article)).unwrap_or(&key).to_owned();
    (!key.is_empty()).then_some(key)
}

fn authors(c: &Connection, edition: i64, work: i64) -> Result<Vec<(i64, String)>, MetadataError> {
    let mut result = Vec::new();
    for (table, column, id) in [("edition_author", "edition_id", edition), ("work_author", "work_id", work)] {
        let mut q = c.prepare_cached(&format!("SELECT a.author_id,coalesce(a.name,'') FROM {table} r JOIN author a USING(author_id) WHERE r.{column}=?1 ORDER BY r.position")).map_err(error)?;
        result = q.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<Result<_, _>>().map_err(error)?;
        if !result.is_empty() {
            break;
        }
    }
    Ok(result)
}

fn same_authors(a: &[(i64, String)], b: &[(i64, String)]) -> bool {
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    let mut unused = b.iter().collect::<Vec<_>>();
    for (id, name) in a {
        let keys = author_match_keys(name).into_iter().filter(|k| k.split_whitespace().count() >= 2).collect::<BTreeSet<_>>();
        let Some(index) = unused.iter().position(|(other_id, other_name)| id == other_id || !keys.is_disjoint(&author_match_keys(other_name))) else {
            return false;
        };
        unused.remove(index);
    }
    true
}

pub(crate) fn recover(c: &Connection, result: &mut IsbnClassificationResult) -> Result<(), MetadataError> {
    if result.matches.len() != 1 || result.matches[0].classifications.iter().any(|c| c.scheme == ClassificationScheme::LibraryOfCongress) {
        return Ok(());
    }
    let matched = &result.matches[0];
    let Some(source_work) = matched.open_library_work_id.as_deref().and_then(|s| open_library_id(s, 'W')) else {
        return Ok(());
    };
    let available: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='edition_bibliography')", [], |r| r.get(0)).map_err(error)?;
    if !available {
        return Ok(());
    }
    let mut sources = Vec::new();
    for id in &matched.exact_edition_ids {
        let Some(edition) = open_library_id(id, 'M') else {
            continue;
        };
        let title: Option<String> = c.query_row("SELECT title FROM edition_bibliography WHERE edition_id=?1", [edition], |r| r.get(0)).optional().map_err(error)?;
        let Some(key) = title.as_deref().and_then(title_key) else {
            return Ok(());
        };
        let names = authors(c, edition, source_work)?;
        if names.is_empty() {
            return Ok(());
        }
        sources.push((key, names));
    }
    let Some((key, source_authors)) = sources.first() else {
        return Ok(());
    };
    if sources.iter().any(|(k, a)| k != key || !same_authors(source_authors, a)) {
        return Ok(());
    }
    let mut candidates = BTreeSet::new();
    let mut examined = BTreeSet::new();
    // Exact normalized titles and subtitle suffixes use the existing title indexes.
    for prefix in ["", "a ", "an ", "the "] {
        let normalized = format!("{prefix}{key}");
        for (table, id, join) in [("edition_bibliography", "b.edition_id", ""), ("work_bibliography", "e.edition_id", " JOIN edition e ON e.work_id=b.work_id")] {
            let sql = format!("SELECT {id} FROM {table} b{join} WHERE b.normalized_title=?1 OR (b.normalized_title>=?2 AND b.normalized_title<?3)");
            let mut q = c.prepare_cached(&sql).map_err(error)?;
            let ids = q.query_map(rusqlite::params![normalized, format!("{normalized} "), format!("{normalized}!")], |r| r.get::<_, i64>(0)).map_err(error)?;
            // Stream the indexed title results, but count only author-matched
            // editions toward the ambiguity limit. A common title must not let
            // unrelated authors crowd out the requested book.
            for id in ids {
                let id = id.map_err(error)?;
                if !examined.insert(id) {
                    continue;
                }
                let work: Option<i64> = c.query_row("SELECT work_id FROM edition WHERE edition_id=?1", [id], |r| r.get(0)).map_err(error)?;
                let Some(work) = work.filter(|w| *w != source_work) else {
                    continue;
                };
                if !same_authors(source_authors, &authors(c, id, work)?) {
                    continue;
                }
                candidates.insert(id);
                if candidates.len() > CANDIDATE_LIMIT {
                    return Ok(());
                }
            }
        }
    }
    let mut evidence = BTreeMap::<String, BTreeSet<metadata_contract::ClassificationEvidence>>::new();
    for edition in candidates {
        let row: Option<(i64, String)> =
            c.query_row("SELECT e.work_id,b.title FROM edition e JOIN edition_bibliography b USING(edition_id) WHERE e.edition_id=?1 AND e.work_id IS NOT NULL", [edition], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(error)?;
        let Some((work, title)) = row else {
            continue;
        };
        if work == source_work || title_key(&title).as_ref() != Some(key) || !same_authors(source_authors, &authors(c, edition, work)?) {
            continue;
        }
        let mut q = c.prepare_cached("SELECT isbn13 FROM edition_isbn WHERE edition_id=?1 ORDER BY isbn13 LIMIT 33").map_err(error)?;
        let isbns = q.query_map([edition], |r| r.get::<_, i64>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
        if isbns.len() > 32 {
            return Ok(());
        }
        for isbn in isbns {
            // Raw lookup only: inferred evidence cannot recursively endorse itself.
            let lookup = crate::lookup_one(c, isbn.to_string())?;
            if lookup.matches.len() != 1 || lookup.matches[0].open_library_work_id.as_deref() != Some(&format!("OL{work}W")) {
                continue;
            }
            let codes =
                lookup.matches[0].classifications.iter().filter(|c| c.scheme == ClassificationScheme::LibraryOfCongress).filter_map(|c| metadata_contract::classification_consensus_notation(c.scheme, &c.notation)).collect::<BTreeSet<_>>();
            if codes.is_empty() {
                continue;
            }
            for code in codes {
                evidence.entry(code).or_default().insert(metadata_contract::ClassificationEvidence {
                    method: "duplicate_work".into(),
                    isbn13: isbn.to_string(),
                    open_library_work_id: format!("OL{work}W"),
                    open_library_edition_id: format!("OL{edition}M"),
                });
            }
        }
    }
    for (notation, records) in evidence {
        result.matches[0].classifications.push(Classification { scheme: ClassificationScheme::LibraryOfCongress, notation, source: ClassificationSource::Work, evidence: records.into_iter().collect() });
    }
    result.matches[0].classifications.sort();
    Ok(())
}
