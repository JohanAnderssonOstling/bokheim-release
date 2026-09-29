//! Exact ISBN associations and conservative cross-authority author reconciliation.
use crate::{error, open_library_id, Connection, MetadataError};
use metadata_contract::{matching::author_names_compatible, *};
use rusqlite::OptionalExtension;
use std::collections::{BTreeMap, BTreeSet};

fn json<T: serde::de::DeserializeOwned>(text: String) -> Result<T, MetadataError> {
    serde_json::from_str(&text).map_err(error)
}

pub(crate) fn profile(c: &Connection, qid: &str, source: &str) -> Result<WikidataAuthorEvidence, MetadataError> {
    let row = c
        .query_row("SELECT name,aliases,birth_year,death_year,identifiers,image FROM profile WHERE qid=?1", [qid], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get(2)?, r.get(3)?, r.get::<_, String>(4)?, r.get(5)?)))
        .optional()
        .map_err(error)?;
    let mut author = WikidataAuthorEvidence { wikidata_id: qid.into(), source_entity: source.into(), identity_match: "unlinked".into(), ..Default::default() };
    if let Some((name, aliases, birth, death, ids, image)) = row {
        author.name = name;
        author.aliases = json::<Vec<String>>(aliases)?.into_iter().take(64).collect();
        author.birth_year = birth;
        author.death_year = death;
        author.identifiers = json(ids)?;
        author.image = image;
    }
    Ok(author)
}
fn books(c: &Connection, isbn: &str) -> Result<Vec<WikidataBookEvidence>, MetadataError> {
    let mut stmt = c.prepare_cached("SELECT book FROM isbn WHERE isbn13=?1 ORDER BY book LIMIT 65").map_err(error)?;
    let ids = stmt.query_map([isbn], |r| r.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
    if ids.len() > 64 {
        return Err(MetadataError("Wikidata ISBN candidate limit exceeded; refusing a partial identity result".into()));
    }
    let mut result = Vec::new();
    for id in ids {
        let title = c.query_row("SELECT name,description FROM profile WHERE qid=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(error)?.unwrap_or((None, None));
        let mut work_stmt = c.prepare_cached("SELECT work FROM edition_work WHERE book=?1 ORDER BY work").map_err(error)?;
        let work_ids = work_stmt.query_map([&id], |r| r.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
        let mut book = WikidataBookEvidence { wikidata_id: id.clone(), work_ids, title: title.0, description: title.1, ..Default::default() };
        let mut stmt = c
            .prepare_cached("SELECT DISTINCT a.author,a.source,c.position FROM isbn_author a LEFT JOIN credit c ON c.book=a.source AND c.author=a.author WHERE a.isbn13=?1 AND a.book=?2 ORDER BY c.position,a.author,a.source LIMIT 129")
            .map_err(error)?;
        let ids = stmt.query_map([isbn, &id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
        if ids.len() > 128 {
            return Err(MetadataError("Wikidata author candidate limit exceeded".into()));
        }
        for (qid, source) in ids {
            book.authors.push(profile(c, &qid, &source)?);
        }
        // Keep all source codes as evidence. Topic codes never become exact
        // book classifications, even when the topic shares the book's label.
        let mut stmt=c.prepare_cached("SELECT scheme,notation,source,property,0 FROM classification WHERE book=?1 UNION SELECT c.scheme,c.notation,c.source,c.property,1 FROM topic_link t JOIN classification c ON c.book=t.topic WHERE t.book=?1 AND c.topic_scope=1 UNION SELECT c.scheme,c.notation,c.source,c.property,0 FROM edition_work w JOIN classification c ON c.book=w.work WHERE w.book=?1 LIMIT 129").map_err(error)?;
        book.classifications = stmt
            .query_map([&id], |r| Ok(WikidataClassificationEvidence { scheme: r.get(0)?, notation: r.get(1)?, source_entity: r.get(2)?, property: r.get(3)?, inferred: r.get(4)? }))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        if book.classifications.len() > 128 {
            return Err(MetadataError("Wikidata classification evidence limit exceeded".into()));
        }
        result.push(book);
    }
    Ok(result)
}
fn authority(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "viaf" => Some("viaf"),
        "isni" => Some("isni"),
        "orcid" => Some("orcid"),
        "lc" | "lccn" => Some("lc"),
        "wikidata" => Some("wikidata"),
        "openlibrary" => Some("openlibrary"),
        _ => None,
    }
}
fn id_key(authority: &str, value: &str) -> Option<String> {
    let value = value.trim();
    let value = if value.starts_with("https://") || value.starts_with("http://") {
        let allowed = match authority {
            "viaf" => "viaf.org/viaf/",
            "isni" => "isni.org/isni/",
            "orcid" => "orcid.org/",
            "lc" => "id.loc.gov/authorities/names/",
            "openlibrary" => "openlibrary.org/authors/",
            "wikidata" => "www.wikidata.org/entity/",
            _ => return None,
        };
        value.strip_prefix("https://").or_else(|| value.strip_prefix("http://"))?.strip_prefix(allowed)?.trim_end_matches('/')
    } else {
        value
    };
    if !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c.is_ascii_whitespace()) {
        return None;
    }
    let key = value.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let positive_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.bytes().any(|b| b != b'0');
    let checked_person_id = |s: &str| {
        if s.len() != 16 || !s.as_bytes()[..15].iter().all(u8::is_ascii_digit) {
            return false;
        }
        let sum = s.as_bytes()[..15].iter().fold(0u32, |sum, b| (sum + u32::from(b - b'0')) * 2);
        let check = (12 - sum % 11) % 11;
        s.as_bytes()[15] == if check == 10 { b'x' } else { b'0' + check as u8 }
    };
    let valid = match authority {
        "viaf" => key.len() <= 22 && positive_digits(&key),
        "isni" | "orcid" => checked_person_id(&key),
        "wikidata" => key.strip_prefix('q').is_some_and(positive_digits),
        "openlibrary" => key.strip_prefix("ol").and_then(|s| s.strip_suffix('a')).is_some_and(positive_digits),
        "lc" => {
            let prefix = key.bytes().take_while(u8::is_ascii_alphabetic).count();
            key.starts_with('n') && (1..=3).contains(&prefix) && (6..=12).contains(&(key.len() - prefix)) && positive_digits(&key[prefix..])
        }
        _ => false,
    };
    valid.then_some(key)
}
fn identifiers<'a>(ids: impl Iterator<Item = (&'a str, &'a str)>) -> BTreeMap<&'static str, BTreeSet<String>> {
    let mut map = BTreeMap::new();
    for (a, v) in ids {
        if let Some(a) = authority(a) {
            if let Some(key) = id_key(a, v) {
                map.entry(a).or_insert_with(BTreeSet::new).insert(key);
            }
        }
    }
    map
}
fn authority_evidence(ol: &AuthorMetadata, wd: &WikidataAuthorEvidence) -> (bool, bool) {
    let left = identifiers(ol.identifiers.iter().map(|i| (i.authority.as_str(), i.value.as_str())).chain(std::iter::once(("openlibrary", ol.open_library_author_id.as_str()))));
    let right = identifiers(wd.identifiers.iter().map(|i| (i.authority.as_str(), i.value.as_str())).chain(std::iter::once(("wikidata", wd.wikidata_id.as_str()))));
    let mut shared = false;
    let mut conflict = false;
    for (a, ids) in left {
        if let Some(other) = right.get(a) {
            if ids.is_disjoint(other) {
                conflict = true;
            } else {
                shared = true;
            }
        }
    }
    (shared, conflict)
}
fn dates_and_names(c: &Connection, ol: &AuthorMetadata, wd: &WikidataAuthorEvidence) -> Result<(bool, bool), MetadataError> {
    let id = open_library_id(&ol.open_library_author_id, 'A').unwrap_or(0);
    let row = c.query_row("SELECT aliases,birth_year,death_year FROM ol_profile WHERE author_id=?1", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i32>>(1)?, r.get::<_, Option<i32>>(2)?))).optional().map_err(error)?;
    let (aliases, birth, death) = row.map(|(a, b, d)| json(a).map(|a: Vec<String>| (a, b, d))).transpose()?.unwrap_or_default();
    let conflict = birth.zip(wd.birth_year).is_some_and(|(a, b)| a != b) || death.zip(wd.death_year).is_some_and(|(a, b)| a != b);
    let compatible = ol.name.iter().chain(aliases.iter()).any(|a| wd.name.iter().chain(wd.aliases.iter()).any(|b| author_names_compatible(a, b)));
    Ok((compatible, conflict))
}
fn shared_works(c: &Connection, core: &Connection, ol: &str, wd: &str) -> Result<u32, MetadataError> {
    let Some(author) = open_library_id(ol, 'A') else { return Ok(0) };
    let mut stmt = c.prepare_cached("SELECT DISTINCT isbn13,work FROM isbn_author WHERE author=?1 ORDER BY work,isbn13 LIMIT 201").map_err(error)?;
    let rows = stmt.query_map([wd], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
    // A bounded sample can establish independent supporting works, never an
    // absence of evidence. Count each Open Library and Wikidata work once.
    let mut pairs = BTreeMap::<String, BTreeSet<i64>>::new();
    let mut query=core.prepare_cached("SELECT DISTINCT e.work_id FROM edition_isbn i JOIN edition e ON e.edition_id=i.edition_id WHERE i.isbn13=?1 AND e.work_id IS NOT NULL AND (EXISTS(SELECT 1 FROM edition_author a WHERE a.edition_id=e.edition_id AND a.author_id=?2) OR EXISTS(SELECT 1 FROM work_author a WHERE a.work_id=e.work_id AND a.author_id=?2))").map_err(error)?;
    for (isbn, work) in rows.into_iter().take(200) {
        let Some(isbn) = crate::canonical_isbn13(&isbn) else { continue };
        for id in query.query_map(rusqlite::params![isbn, author], |r| r.get::<_, i64>(0)).map_err(error)? {
            pairs.entry(work.clone()).or_default().insert(id.map_err(error)?);
        }
    }
    // Need a one-to-one selection of at least two work pairs. Several ISBNs
    // or duplicate records of one work must never count as multiple books.
    let mut count = 0;
    let mut used = BTreeSet::new();
    for ids in pairs.values() {
        if let Some(id) = ids.iter().find(|id| !used.contains(*id)) {
            used.insert(*id);
            count += 1;
        }
    }
    Ok(count)
}
pub(crate) fn reconcile(c: &Connection, core: &Connection, authors: &mut [AuthorMetadata], books: &mut [WikidataBookEvidence]) -> Result<(), MetadataError> {
    let mut evidence = BTreeMap::<String, WikidataAuthorEvidence>::new();
    for author in books.iter().flat_map(|b| &b.authors) {
        evidence.entry(author.wikidata_id.clone()).or_insert_with(|| author.clone());
    }
    let mut proposals = BTreeMap::<String, Vec<(usize, &'static str, u32)>>::new();
    for (qid, wd) in &evidence {
        for (index, ol) in authors.iter().enumerate() {
            if open_library_id(&ol.open_library_author_id, 'A').is_none() {
                continue;
            }
            let (shared, id_conflict) = authority_evidence(ol, wd);
            let (name, date_conflict) = dates_and_names(c, ol, wd)?;
            if !shared && !name {
                continue;
            }
            let count = if name && !id_conflict && !date_conflict && !shared { shared_works(c, core, &ol.open_library_author_id, qid)? } else { 0 };
            let strong_name = ol.name.as_deref().is_some_and(|n| crate::import::core::author_match_tokens(n).len() >= 2);
            let kind = if id_conflict || date_conflict {
                "conflict"
            } else if shared {
                "authority"
            } else if count >= 2 && strong_name {
                "corroborated"
            } else {
                "provisional"
            };
            proposals.entry(qid.clone()).or_default().push((index, kind, count));
        }
    }
    // Never pair by author-list position; require unique evidence both ways.
    let mut choices = BTreeMap::new();
    for (qid, options) in &proposals {
        let strong = options.iter().filter(|(_, kind, _)| matches!(*kind, "authority" | "corroborated")).collect::<Vec<_>>();
        let chosen = if strong.len() == 1 {
            Some(*strong[0])
        } else if options.len() == 1 {
            Some(options[0])
        } else {
            None
        };
        if let Some(choice) = chosen {
            choices.insert(qid.clone(), choice);
        }
    }
    for book in books {
        for wd in &mut book.authors {
            if let Some(&(index, kind, count)) = choices.get(&wd.wikidata_id) {
                let unique = choices.values().filter(|(i, k, _)| *i == index && matches!(*k, "authority" | "corroborated")).count() == 1;
                let accepted = matches!(kind, "authority" | "corroborated") && unique;
                wd.identity_match = if matches!(kind, "authority" | "corroborated") && !unique { "provisional" } else { kind }.into();
                wd.open_library_author_id = Some(authors[index].open_library_author_id.clone());
                wd.shared_work_count = count;
                if accepted {
                    let id = AuthorIdentifier { authority: "wikidata".into(), value: wd.wikidata_id.clone() };
                    if !authors[index].identifiers.contains(&id) {
                        authors[index].identifiers.push(id);
                    }
                }
            } else if proposals.contains_key(&wd.wikidata_id) {
                wd.identity_match = "ambiguous".into();
            }
        }
    }
    Ok(())
}
fn wd_author(author: &WikidataAuthorEvidence, position: usize) -> AuthorMetadata {
    AuthorMetadata {
        open_library_author_id: String::new(),
        name: author.name.clone(),
        identifiers: vec![AuthorIdentifier { authority: "wikidata".into(), value: author.wikidata_id.clone() }],
        source: AuthorSource::Work,
        position: position as u32,
    }
}
pub(crate) fn direct_codes(book: &WikidataBookEvidence, isbn: &str) -> Vec<Classification> {
    book.classifications
        .iter()
        .filter(|c| !c.inferred)
        .filter_map(|c| {
            let scheme = match c.scheme.as_str() {
                "lcc" => ClassificationScheme::LibraryOfCongress,
                "ddc" => ClassificationScheme::DeweyDecimal,
                "bisac" => ClassificationScheme::Bisac,
                _ => return None,
            };
            let notation = metadata_contract::classification_consensus_notation(scheme, &c.notation)?;
            Some(Classification {
                scheme,
                notation,
                source: if c.source_entity == book.wikidata_id { ClassificationSource::ExactEdition } else { ClassificationSource::Work },
                evidence: vec![ClassificationEvidence { method: format!("wikidata:direct:{}:{}", c.source_entity, c.property), isbn13: isbn.into(), open_library_work_id: String::new(), open_library_edition_id: String::new() }],
            })
        })
        .collect()
}

pub(crate) fn enrich(c: &Connection, core: &Connection, response: &mut RichMetadataEnrichmentResponse) -> Result<(), MetadataError> {
    for result in &mut response.results {
        let Some(isbn) = result.canonical_isbn13.as_deref() else { continue };
        let mut evidence = books(c, isbn)?;
        if evidence.is_empty() {
            continue;
        }
        if result.matches.is_empty() {
            // Preserve distinct ISBN claimants as separate candidate records.
            // Their QIDs are exposed in wikidata_books, not forged OL IDs.
            for book in &evidence {
                result.matches.push(RichWorkMetadataMatch {
                    open_library_work_id: None,
                    exact_edition_ids: Vec::new(),
                    classifications: direct_codes(book, isbn),
                    authors: book.authors.iter().enumerate().map(|(i, a)| wd_author(a, i)).collect(),
                    description: book.description.clone(),
                    subjects: Vec::new(),
                });
            }
            result.status = if result.matches.len() == 1 { LookupStatus::Matched } else { LookupStatus::Ambiguous };
        } else if result.matches.len() == 1 {
            let matched = &mut result.matches[0];
            reconcile(c, core, &mut matched.authors, &mut evidence)?;
            // Only a unique Wikidata ISBN claimant may fill missing fields.
            // Multiple claimants remain visible for review without guessing.
            if evidence.len() == 1 {
                let book = &evidence[0];
                let existing = matched.classifications.iter().filter(|c| classification_consensus_notation(c.scheme, &c.notation).is_some()).map(|c| c.scheme).collect::<BTreeSet<_>>();
                matched.classifications.extend(direct_codes(book, isbn).into_iter().filter(|c| !existing.contains(&c.scheme)));
                if matched.authors.is_empty() {
                    matched.authors = book.authors.iter().enumerate().map(|(i, a)| wd_author(a, i)).collect();
                }
                if matched.description.as_deref().is_none_or(|d| d.trim().is_empty()) {
                    matched.description = book.description.clone();
                }
                if result.status == LookupStatus::NoMatch {
                    result.status = LookupStatus::Matched;
                }
            }
        }
        result.wikidata_books = evidence;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn index() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../wikidata-author-schema.sql")).unwrap();
        c
    }
    fn ol(name: &str, id: &str) -> AuthorMetadata {
        AuthorMetadata { open_library_author_id: id.into(), name: Some(name.into()), identifiers: Vec::new(), source: AuthorSource::Work, position: 0 }
    }
    fn wd(name: &str, qid: &str) -> WikidataAuthorEvidence {
        WikidataAuthorEvidence { name: Some(name.into()), wikidata_id: qid.into(), identity_match: "unlinked".into(), ..Default::default() }
    }
    fn book(author: WikidataAuthorEvidence) -> Vec<WikidataBookEvidence> {
        vec![WikidataBookEvidence { wikidata_id: "Q10".into(), authors: vec![author], ..Default::default() }]
    }
    fn core() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE edition(edition_id INTEGER,work_id INTEGER); CREATE TABLE edition_isbn(edition_id INTEGER,isbn13 INTEGER); CREATE TABLE edition_author(edition_id INTEGER,author_id INTEGER); CREATE TABLE work_author(work_id INTEGER,author_id INTEGER);").unwrap();
        c
    }
    #[test]
    fn placeholder_and_invalid_authority_ids_cannot_establish_identity() {
        for authority in ["viaf", "isni", "orcid", "lc", "wikidata", "openlibrary"] {
            assert_eq!(id_key(authority, "unknown"), None);
        }
        assert_eq!(id_key("isni", "0000000122819550"), None);
        assert_eq!(id_key("viaf", "https://unrelated.example/123"), None);
        assert_eq!(id_key("openlibrary", "OL123M"), None);
        assert_eq!(id_key("lc", "n79021164"), Some("n79021164".into()));
        let mut author = ol("Different Person", "OL1A");
        author.identifiers.push(AuthorIdentifier { authority: "viaf".into(), value: "unknown".into() });
        let mut remote = wd("Another Person", "Q1");
        remote.identifiers.push(WikidataAuthorityId { authority: "viaf".into(), value: "unknown".into() });
        assert_eq!(authority_evidence(&author, &remote), (false, false));
    }
    #[test]
    fn authority_links_accept_aliases_but_dates_and_authority_conflicts_veto() {
        let c = index();
        let core = core();
        let mut authors = vec![ol("Pen Name", "OL1A")];
        authors[0].identifiers.push(AuthorIdentifier { authority: "isni".into(), value: "0000 0001 2281 955X".into() });
        let mut author = wd("Different legal name", "Q1");
        author.identifiers.push(WikidataAuthorityId { authority: "isni".into(), value: "000000012281955X".into() });
        let mut books = book(author.clone());
        reconcile(&c, &core, &mut authors, &mut books).unwrap();
        assert_eq!(books[0].authors[0].identity_match, "authority");
        assert!(authors[0].identifiers.iter().any(|i| i.authority == "wikidata"));
        authors[0].identifiers.retain(|i| i.authority != "wikidata");
        c.execute("INSERT INTO ol_profile VALUES(1,'[]',1900,NULL)", []).unwrap();
        author.birth_year = Some(1950);
        let mut books = book(author.clone());
        reconcile(&c, &core, &mut authors, &mut books).unwrap();
        assert_eq!(books[0].authors[0].identity_match, "conflict");
        assert_eq!(authors[0].identifiers.len(), 1);
        c.execute("DELETE FROM ol_profile", []).unwrap();
        author.birth_year = None;
        author.identifiers.push(WikidataAuthorityId { authority: "viaf".into(), value: "999".into() });
        authors[0].identifiers.push(AuthorIdentifier { authority: "viaf".into(), value: "111".into() });
        let mut books = book(author);
        reconcile(&c, &core, &mut authors, &mut books).unwrap();
        assert_eq!(books[0].authors[0].identity_match, "conflict");
    }
    #[test]
    fn shared_isbn_names_are_provisional_and_editions_do_not_inflate_work_count() {
        let c = index();
        let core = core();
        let mut authors = vec![ol("Alex Smith", "OL1A")];
        c.execute_batch("INSERT INTO isbn_author VALUES('9780306406157','Q10','Q100','Q1','Q100'),('9780821338278','Q11','Q100','Q1','Q100');").unwrap();
        core.execute_batch("INSERT INTO edition VALUES(1,10),(2,10);INSERT INTO edition_isbn VALUES(1,9780306406157),(2,9780821338278);INSERT INTO work_author VALUES(10,1);").unwrap();
        let mut books = book(wd("Smith, Alex", "Q1"));
        reconcile(&c, &core, &mut authors, &mut books).unwrap();
        assert_eq!(books[0].authors[0].identity_match, "provisional");
        assert_eq!(books[0].authors[0].shared_work_count, 1);
        assert!(authors[0].identifiers.is_empty());
        // Two Wikidata records for the same OL work are still one work.
        c.execute("UPDATE isbn_author SET work='Q101' WHERE book='Q11'", []).unwrap();
        assert_eq!(shared_works(&c, &core, "OL1A", "Q1").unwrap(), 1);
        core.execute_batch("UPDATE edition SET work_id=11 WHERE edition_id=2;INSERT INTO work_author VALUES(11,1);").unwrap();
        reconcile(&c, &core, &mut authors, &mut books).unwrap();
        assert_eq!(books[0].authors[0].identity_match, "corroborated");
        assert_eq!(books[0].authors[0].shared_work_count, 2);
    }
    #[test]
    fn multi_author_matching_is_not_positional_and_collisions_do_not_merge() {
        let c = index();
        let core = core();
        let mut authors = vec![ol("Alex Smith", "OL1A"), ol("Jane Jones", "OL2A")];
        authors[0].identifiers.push(AuthorIdentifier { authority: "viaf".into(), value: "100".into() });
        authors[1].identifiers.push(AuthorIdentifier { authority: "viaf".into(), value: "200".into() });
        let mut a = wd("Jane Jones", "Q2");
        a.identifiers.push(WikidataAuthorityId { authority: "viaf".into(), value: "200".into() });
        let mut b = wd("Alex Smith", "Q1");
        b.identifiers.push(WikidataAuthorityId { authority: "viaf".into(), value: "100".into() });
        let mut books = book(a);
        books[0].authors.push(b.clone());
        reconcile(&c, &core, &mut authors, &mut books).unwrap();
        assert_eq!(books[0].authors[0].open_library_author_id.as_deref(), Some("OL2A"));
        for a in &mut authors {
            a.identifiers.retain(|i| i.authority != "wikidata");
        }
        b.wikidata_id = "Q3".into();
        books[0].authors.push(b);
        reconcile(&c, &core, &mut authors, &mut books).unwrap();
        assert!(!authors[0].identifiers.iter().any(|i| i.authority == "wikidata"));
    }
    #[test]
    fn missing_book_is_returned_and_topic_codes_are_not_promoted() {
        let c = index();
        let core = core();
        c.execute_batch("INSERT INTO isbn VALUES('9780306406157','Q10');INSERT INTO profile VALUES('Q10','Book','[]',NULL,NULL,'[]',NULL,'A book description');INSERT INTO profile VALUES('Q1','Alex Smith','[]',1900,1980,'[]','Portrait.jpg',NULL);INSERT INTO isbn_author VALUES('9780306406157','Q10','Q10','Q1','Q10');INSERT INTO classification VALUES('Q10','lcc','QA76.6','P8360',0,'Q10'),('Q20','lcc','BC71','P1149',1,'Q20');INSERT INTO topic_link VALUES('Q10','Q20');").unwrap();
        let mut response = RichMetadataEnrichmentResponse {
            snapshot: SnapshotDescription { dump_date: "test".into(), imported_at_ms: 0 },
            results: vec![RichIsbnMetadataResult { wikidata_books: Vec::new(), requested_isbn: "9780306406157".into(), canonical_isbn13: Some("9780306406157".into()), status: LookupStatus::NoMatch, matches: vec![] }],
        };
        enrich(&c, &core, &mut response).unwrap();
        let result = &response.results[0];
        assert_eq!(result.status, LookupStatus::Matched);
        assert!(result.matches[0].authors[0].open_library_author_id.is_empty());
        assert_eq!(result.matches[0].authors[0].identity_key(), "wikidata:Q1");
        assert_eq!(result.matches[0].classifications.len(), 1);
        assert_eq!(result.wikidata_books[0].classifications.len(), 2);
        let wire = encode_v2_response(&response).unwrap();
        let decoded = decode_v2_response(&wire, MAX_RESPONSE_BYTES).unwrap();
        assert_eq!(decoded.results[0].wikidata_books, response.results[0].wikidata_books);
        response.results[0].matches[0].classifications[0].notation = "QA1".into();
        enrich(&c, &core, &mut response).unwrap();
        assert_eq!(response.results[0].matches[0].classifications[0].notation, "QA1");
    }
}
