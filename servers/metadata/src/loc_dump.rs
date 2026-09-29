//! ISBN/LCC evidence imported into the existing metadata snapshot from LC MARC.
use crate::{error, Classification, ClassificationScheme, ClassificationSource, Connection, MetadataError, WorkClassificationMatch};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn has_lcc_dump(connection: &Connection) -> Result<bool, MetadataError> {
    connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='isbn_lcc')", [], |r| r.get(0)).map_err(error)
}

pub(crate) fn lookup_lcc(connection: &Connection, isbn13: i64) -> Result<BTreeSet<String>, MetadataError> {
    if !has_lcc_dump(connection)? {
        return Ok(BTreeSet::new());
    }
    let mut statement = connection.prepare_cached("SELECT DISTINCT notation FROM isbn_lcc WHERE isbn13=?1 ORDER BY notation").map_err(error)?;
    let rows = statement.query_map([isbn13], |row| row.get::<_, String>(0)).map_err(error)?;
    rows.collect::<Result<BTreeSet<_>, _>>().map_err(error)
}

pub(crate) fn merge_lcc(matches: &mut Vec<WorkClassificationMatch>, codes: BTreeSet<String>) {
    // Do not attach one ISBN's evidence to conflicting Open Library works.
    if matches.len() > 1 {
        return;
    }
    let codes = codes.into_iter().filter_map(|code| subject_projection::canonical_lcc_notation(&code)).collect::<BTreeSet<_>>();
    if codes.is_empty() {
        return;
    }
    if matches.is_empty() {
        // An authority record is not an Open Library edition: never manufacture OL IDs.
        matches.push(WorkClassificationMatch { open_library_work_id: None, exact_edition_ids: Vec::new(), classifications: Vec::new() });
    }
    let classifications = &mut matches[0].classifications;
    for notation in codes {
        if let Some(existing) = classifications.iter_mut().find(|c| c.scheme == ClassificationScheme::LibraryOfCongress && c.notation == notation) {
            existing.source = ClassificationSource::ExactEdition;
        } else {
            classifications.push(Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation, source: ClassificationSource::ExactEdition });
        }
    }
    classifications.sort();
}

/// Fill the rich fields already supported by the wire contract. Bibliographic
/// fields remain in lc_record for a future provider-neutral identity response.
pub(crate) fn enrich(connection: &Connection, response: &mut crate::RichMetadataEnrichmentResponse) -> Result<(), MetadataError> {
    let available: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lc_record')", [], |r| r.get(0)).map_err(error)?;
    if !available {
        return Ok(());
    }
    let mut query = connection.prepare_cached("SELECT r.metadata FROM lc_record_isbn i JOIN lc_record r USING(lc_record_id) WHERE i.isbn13=?1 ORDER BY i.lc_record_id").map_err(error)?;
    #[derive(serde::Deserialize)]
    struct Record {
        #[serde(default)]
        descriptions: Vec<String>,
        #[serde(default)]
        subjects: Vec<crate::SubjectHeading>,
    }
    for result in &mut response.results {
        if result.matches.len() > 1 {
            continue;
        }
        let Some(isbn) = crate::canonical_isbn13(&result.requested_isbn) else { continue };
        let mut descriptions = BTreeSet::new();
        let mut subjects = BTreeSet::new();
        let rows = query.query_map([isbn], |r| r.get::<_, String>(0)).map_err(error)?;
        for row in rows {
            let record: Record = serde_json::from_str(&row.map_err(error)?).map_err(error)?;
            descriptions.extend(record.descriptions.into_iter().filter(|s| !s.trim().is_empty() && s.len() <= crate::MAX_DESCRIPTION_BYTES));
            subjects.extend(record.subjects);
        }
        if descriptions.is_empty() && subjects.is_empty() {
            continue;
        }
        if result.matches.is_empty() {
            result.matches.push(crate::RichWorkMetadataMatch { open_library_work_id: None, exact_edition_ids: Vec::new(), classifications: Vec::new(), authors: Vec::new(), description: None, subjects: Vec::new() });
            result.status = crate::LookupStatus::Matched;
        }
        let matched = &mut result.matches[0];
        if matched.description.as_ref().is_none_or(|s| s.trim().is_empty()) && descriptions.len() == 1 {
            matched.description = descriptions.into_iter().next();
        }
        if matched.subjects.is_empty() {
            matched.subjects = subjects.into_iter().collect();
        }
    }
    Ok(())
}

/// Indexed, classification-only fallback. Never fabricates an edition or ISBN.
pub(crate) fn lookup_title_author(connection: &Connection, query: &crate::EditionIdentityQuery) -> Result<Option<metadata_contract::AuthoritySubjectMatch>, MetadataError> {
    use metadata_contract::matching::{author_credits, matching_author_count, NormalizedTitle};
    let available: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lc_title')", [], |r| r.get(0)).map_err(error)?;
    let authors = author_credits(&query.authors);
    if !available || authors.is_empty() {
        return Ok(None);
    }
    let title = NormalizedTitle::new(&query.title);
    let full = &title.full;
    let main = &title.main;
    if full.is_empty() {
        return Ok(None);
    }
    #[derive(serde::Deserialize)]
    struct Record {
        title: String,
        main_title: String,
        authors: Vec<String>,
    }
    let mut statement = connection.prepare_cached("SELECT t.lc_record_id,r.metadata FROM lc_title t JOIN lc_record r USING(lc_record_id) WHERE t.title_key=?1 ORDER BY t.lc_record_id LIMIT 129").map_err(error)?;
    let mut codes = connection.prepare_cached("SELECT notation FROM lc_record_lcc WHERE lc_record_id=?1 ORDER BY notation").map_err(error)?;
    for key in title.lookup_keys() {
        let records = statement.query_map([key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
        // Never accept consensus from a truncated authority candidate set.
        if records.len() > 128 {
            return Ok(None);
        }
        let mut accepted = Vec::new();
        let mut best_author_count = 0;
        for (id, json) in records {
            let record: Record = serde_json::from_str(&json).map_err(error)?;
            let record_title = NormalizedTitle::new(&record.title);
            let record_full = &record_title.full;
            let record_main = NormalizedTitle::new(&record.main_title).full;
            if full != record_full && full != main && record_full != &record_main {
                continue;
            }
            if title.qualifiers != record_title.qualifiers {
                continue;
            }
            let author_count = matching_author_count(&authors, &record.authors);
            if author_count == 0 || author_count < best_author_count {
                continue;
            }
            let notations = codes.query_map([&id], |r| r.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
            let normalized =
                notations.iter().filter_map(|n| subject_projection::canonical_lcc_notation(n)).filter_map(|n| metadata_contract::classification_consensus_notation(ClassificationScheme::LibraryOfCongress, &n)).collect::<BTreeSet<_>>();
            if normalized.is_empty() {
                continue;
            }
            if author_count > best_author_count {
                accepted.clear();
                best_author_count = author_count;
            }
            accepted.push((id, record, normalized));
        }
        if accepted.is_empty() {
            continue;
        }
        let mut collected = BTreeMap::<String, Vec<metadata_contract::ClassificationEvidence>>::new();
        for (id, _, codes) in &accepted {
            for code in codes {
                collected.entry(code.clone()).or_default().push(metadata_contract::ClassificationEvidence {
                    method: format!("library_of_congress_title_author:{id}"),
                    isbn13: String::new(),
                    open_library_work_id: String::new(),
                    open_library_edition_id: String::new(),
                });
            }
        }
        return Ok(Some(metadata_contract::AuthoritySubjectMatch {
            main_title: accepted[0].1.main_title.clone(),
            provider_id: "library_of_congress".into(),
            record_ids: accepted.iter().map(|(id, _, _)| id.clone()).collect(),
            title: accepted[0].1.title.clone(),
            authors: accepted[0].1.authors.clone(),
            classifications: collected.into_iter().map(|(notation, evidence)| Classification { notation, scheme: ClassificationScheme::LibraryOfCongress, source: ClassificationSource::Work, evidence }).collect(),
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    fn setup() -> (Connection, crate::EditionIdentityQuery) {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE lc_record(lc_record_id TEXT PRIMARY KEY, metadata TEXT); CREATE TABLE lc_record_lcc(lc_record_id TEXT,notation TEXT); CREATE TABLE lc_title(title_key TEXT,lc_record_id TEXT,PRIMARY KEY(title_key,lc_record_id));",
        )
        .unwrap();
        let q = crate::EditionIdentityQuery { query_id: "book".into(), title: "French history".into(), authors: vec!["Jane Smith".into()], publishers: vec![], book_year: None };
        (c, q)
    }
    fn add(c: &Connection, id: &str, title: &str, author: &str, code: &str) {
        let main = title.split(':').next().unwrap();
        let json = serde_json::json!({"title":title,"main_title":main,"authors":[author]}).to_string();
        c.execute("INSERT INTO lc_record VALUES(?1,?2)", (id, json)).unwrap();
        c.execute("INSERT INTO lc_record_lcc VALUES(?1,?2)", (id, code)).unwrap();
        for key in [title, main].map(metadata_contract::matching::bibliographic_match_key) {
            c.execute("INSERT OR IGNORE INTO lc_title VALUES(?1,?2)", (key, id)).unwrap();
        }
    }
    #[test]
    fn authority_lookup_prefers_more_matching_authors_and_preserves_ties() {
        let (c, mut q) = setup();
        q.authors.push("Casey Roe".into());
        add(&c, "one", "French history", "Jane Smith", "DC38");
        add(&c, "two", "French history", "Jane Smith", "DC161");
        let metadata = serde_json::json!({"title":"French history","main_title":"French history","authors":["Jane Smith","Casey Roe","Narrator Person"]}).to_string();
        c.execute("UPDATE lc_record SET metadata=?1 WHERE lc_record_id='two'", [metadata]).unwrap();
        assert_eq!(lookup_title_author(&c, &q).unwrap().unwrap().classifications[0].notation, "DC161");
        q.authors.pop();
        assert_eq!(lookup_title_author(&c, &q).unwrap().unwrap().classifications.len(), 2);
    }
    #[test]
    fn annotated_title_survives_authority_lookup_and_client_verification() {
        let (c, mut q) = setup();
        q.title = "Albion's Seed (Unabridged)".into();
        q.authors = vec!["David Hackett Fischer".into()];
        add(&c, "89016069", "Albion's seed: four British folkways in America", "Fischer, David Hackett,", "E162");
        let matched = lookup_title_author(&c, &q).unwrap().unwrap();
        assert!(metadata_contract::authority_subjects::classification_record(&q, &matched).is_some());
        q.title = "Albion's Seed (Abridged)".into();
        assert!(lookup_title_author(&c, &q).unwrap().is_none());
        q.title = "Albion's Seed (Volume 2)".into();
        assert!(lookup_title_author(&c, &q).unwrap().is_none());
    }
    #[test]
    fn no_isbn_authority_match_preserves_control_numbers_without_ol_identity() {
        let (c, q) = setup();
        add(&c, "lc1", "French history", "Smith, Jane", "DC161 .S5 1900");
        add(&c, "lc2", "French history", "Jane Smith", "DC161 .S8 2004");
        let m = lookup_title_author(&c, &q).unwrap().unwrap();
        assert_eq!(m.record_ids, vec!["lc1", "lc2"]);
        let record = metadata_contract::authority_subjects::classification_record(&q, &m).unwrap();
        assert_eq!(record.classifications[0].notation, "DC161");
        assert_eq!(record.classifications[0].evidence.len(), 2);
        assert!(record.open_library_work_id.is_none() && record.exact_edition_ids.is_empty());
    }
    #[test]
    fn differing_codes_are_kept_but_wrong_authors_and_title_only_do_not_classify() {
        let (c, mut q) = setup();
        add(&c, "lc1", "French history", "Jane Smith", "DC161");
        add(&c, "lc2", "French history", "Jane Smith", "DC38");
        let matched = lookup_title_author(&c, &q).unwrap().unwrap();
        assert_eq!(matched.classifications.len(), 2);
        let verified = metadata_contract::authority_subjects::classification_record(&q, &matched).unwrap();
        assert!(verified.classifications.iter().all(|c| c.evidence.len() == 1));
        assert_ne!(verified.classifications[0].evidence[0].method, verified.classifications[1].evidence[0].method);
        c.execute("DELETE FROM lc_title WHERE lc_record_id='lc2'", []).unwrap();
        q.authors = vec!["John Smith".into()];
        assert!(lookup_title_author(&c, &q).unwrap().is_none());
        q.authors.clear();
        assert!(lookup_title_author(&c, &q).unwrap().is_none());
    }
    #[test]
    fn main_title_fallback_does_not_discard_conflicting_subtitles_or_volumes() {
        let (c, mut q) = setup();
        add(&c, "lc1", "French history: a survey", "Jane Smith", "DC161");
        assert!(lookup_title_author(&c, &q).unwrap().is_some());
        q.title = "French history: a biography".into();
        assert!(lookup_title_author(&c, &q).unwrap().is_none());
        q.title = "French history: volume 2".into();
        assert!(lookup_title_author(&c, &q).unwrap().is_none());
    }
    #[test]
    fn truncation_never_establishes_authority_consensus() {
        let (c, q) = setup();
        for n in 0..129 {
            add(&c, &format!("lc{n}"), "French history", "Jane Smith", "DC161");
        }
        assert!(lookup_title_author(&c, &q).unwrap().is_none());
    }
    #[test]
    fn old_snapshot_without_authority_tables_remains_supported() {
        let c = Connection::open_in_memory().unwrap();
        assert!(lookup_title_author(&c, &setup().1).unwrap().is_none());
    }
}
