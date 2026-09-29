use super::{
    classification_similarity_keys, error, sql_placeholders, BTreeMap, BTreeSet, Classification, ClassificationScheme, ClassificationSource, Connection, EditionIdentityMatch, EditionIdentityQuery, EditionIdentityResult,
    EditionIdentityStatus, EditionTitleMatch, HashMap, MetadataError, BULK_EVIDENCE_IDS_PER_QUERY, LCC_SCHEME, MAX_EDITION_IDENTITY_REVIEW_CANDIDATES, MAX_RAW_TITLE_CANDIDATES, MAX_REVERSE_TITLE_CANDIDATES,
};
use crate::import::core::{author_match_keys, bibliographic_match_key};

struct PreparedEditionQuery {
    query: EditionIdentityQuery,
    author_keys: BTreeSet<String>,
    publisher_keys: BTreeSet<String>,
}

struct EditionCandidate {
    edition_id: i64,
    work_id: Option<i64>,
    edition_title: Option<String>,
    subtitle: Option<String>,
    work_title: Option<String>,
    book_year: Option<i32>,
    title_specificity: usize,
    matched_edition_title: bool,
    matched_work_title: bool,
}

pub(crate) fn resolve_edition_batch(connection: &Connection, queries: Vec<EditionIdentityQuery>) -> Result<Vec<EditionIdentityResult>, MetadataError> {
    let mut prepared = Vec::<Option<PreparedEditionQuery>>::with_capacity(queries.len());
    let mut completed = Vec::<Option<EditionIdentityResult>>::with_capacity(queries.len());
    let mut query_title_keys = Vec::<Vec<(String, usize)>>::with_capacity(queries.len());
    for query in queries {
        if query.query_id.is_empty() || query.query_id.len() > 256 || query.title.len() > 4_096 || query.authors.len() > 32 || query.publishers.len() > 32 {
            return Err(MetadataError("edition query contains too many or oversized fields".to_owned()));
        }
        let title_keys = identity_title_keys(&query.title);
        let author_keys = query.authors.iter().flat_map(|author| author_match_keys(author)).collect::<BTreeSet<_>>();
        let publisher_keys = query.publishers.iter().map(|publisher| bibliographic_match_key(publisher)).filter(|key| !key.is_empty()).collect::<BTreeSet<_>>();
        if title_keys.is_empty() {
            completed.push(Some(identity_result(&query.query_id, EditionIdentityStatus::InsufficientMetadata, None, Vec::new(), false)));
            prepared.push(None);
            query_title_keys.push(Vec::new());
            continue;
        }
        query_title_keys.push(title_keys);
        completed.push(None);
        prepared.push(Some(PreparedEditionQuery { query, author_keys, publisher_keys }));
    }

    let mut title_keys = query_title_keys.into_iter().map(Vec::into_iter).collect::<Vec<_>>();
    let has_title_author_classification: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='title_author_classification')", [], |row| row.get(0)).map_err(error)?;
    // Resolve each specificity tier before deciding whether to broaden a query.
    // Evidence reads stay batched across the queries still needing a result.
    while completed.iter().any(Option::is_none) {
        let mut candidates = BTreeMap::new();
        for (query_index, keys) in title_keys.iter_mut().enumerate() {
            if completed[query_index].is_some() {
                continue;
            }
            let prepared_query = prepared[query_index].as_ref().expect("unfinished query is prepared");
            let Some((title, specificity)) = keys.next() else {
                completed[query_index] = Some(identity_result(&prepared_query.query.query_id, EditionIdentityStatus::NoMatch, None, Vec::new(), false));
                continue;
            };
            let mut matched = load_edition_candidates(connection, &[(query_index, title, specificity)])?;
            if let Some(mut values) = matched.remove(&query_index) {
                let qualifiers = title_qualifiers(&prepared_query.query.title);
                values.retain(|candidate| candidate.edition_title.as_deref().or(candidate.work_title.as_deref()).is_some_and(|title| title_qualifiers(title) == qualifiers));
                if values.len() > MAX_REVERSE_TITLE_CANDIDATES {
                    // An incomplete candidate set cannot safely justify fallback.
                    completed[query_index] = Some(identity_result(&prepared_query.query.query_id, EditionIdentityStatus::Ambiguous, None, Vec::new(), true));
                } else if !values.is_empty() {
                    candidates.insert(query_index, values);
                }
            }
        }
        if candidates.is_empty() {
            continue;
        }
        let edition_ids = candidates.values().flatten().map(|candidate| candidate.edition_id).collect::<BTreeSet<_>>();
        let work_ids = candidates.values().flatten().filter_map(|candidate| candidate.work_id).collect::<BTreeSet<_>>();
        let edition_authors = load_edition_authors(connection, &edition_ids)?;
        let work_authors = load_work_authors(connection, &work_ids)?;
        let publishers = load_edition_publishers(connection, &edition_ids)?;
        let isbns = load_edition_isbns(connection, &edition_ids)?;
        let edition_classifications = load_classifications_by_ids(connection, "edition_classification", "edition_id", &edition_ids)?;
        let work_classifications = load_classifications_by_ids(connection, "work_classification", "work_id", &work_ids)?;
        let sibling_classifications = load_classifications_by_ids(connection, "edition_work_classification", "work_id", &work_ids)?;

        for (query_index, query_candidates) in candidates {
            let prepared_query = prepared[query_index].as_ref().expect("unfinished query is prepared");
            let result = resolve_prepared_edition_query(
                connection,
                has_title_author_classification,
                prepared_query,
                query_candidates,
                &edition_authors,
                &work_authors,
                &publishers,
                &isbns,
                &edition_classifications,
                &work_classifications,
                &sibling_classifications,
            )?;
            // Only rejected/unusable candidates permit a broader search. A
            // credible ambiguity must not be replaced by a weaker title hit.
            if result.status != EditionIdentityStatus::NoMatch {
                completed[query_index] = Some(result);
            }
        }
    }
    Ok(completed.into_iter().map(|result| result.expect("every edition query is completed")).collect())
}

pub(crate) fn edition_candidate_query(title_rows: usize, year_column: &'static str) -> String {
    let values = (0..title_rows).map(|_| "(?,?,?)").collect::<Vec<_>>().join(",");
    format!(
        "WITH request_title(query_index,normalized_title,specificity) AS (VALUES {values}),
         edition_candidate_source(query_index,edition_id,specificity,edition_match,work_match) AS (
             SELECT request.query_index,bibliography.edition_id,request.specificity,1,0
             FROM request_title request
             CROSS JOIN edition_bibliography AS bibliography INDEXED BY edition_bibliography_by_title
               ON bibliography.normalized_title=request.normalized_title
             WHERE EXISTS (SELECT 1 FROM edition_classification WHERE edition_id=bibliography.edition_id)
                OR EXISTS (
                    SELECT 1
                    FROM edition
                    WHERE edition.edition_id=bibliography.edition_id
                      AND (EXISTS (SELECT 1 FROM work_classification WHERE work_id=edition.work_id)
                        OR EXISTS (SELECT 1 FROM edition_work_classification WHERE work_id=edition.work_id))
                )
             LIMIT ?
         ),
         work_candidate_source(query_index,edition_id,specificity,edition_match,work_match) AS (
             SELECT request.query_index,edition.edition_id,request.specificity,0,1
             FROM request_title request
             CROSS JOIN work_bibliography AS work_bibliography INDEXED BY work_bibliography_by_title
               ON work_bibliography.normalized_title=request.normalized_title
             CROSS JOIN edition AS edition INDEXED BY edition_by_work ON edition.work_id=work_bibliography.work_id
             WHERE EXISTS (SELECT 1 FROM edition_classification WHERE edition_id=edition.edition_id)
                OR EXISTS (SELECT 1 FROM work_classification WHERE work_id=edition.work_id)
                OR EXISTS (SELECT 1 FROM edition_work_classification WHERE work_id=edition.work_id)
             LIMIT ?
         ),
         candidate_source(query_index,edition_id,specificity,edition_match,work_match) AS (
             SELECT * FROM edition_candidate_source
             UNION ALL
             SELECT * FROM work_candidate_source
         ),
         candidate_specificity AS (
             SELECT query_index,edition_id,MAX(specificity) AS specificity
             FROM candidate_source GROUP BY query_index,edition_id
         ),
         candidate AS (
             SELECT source.query_index,source.edition_id,score.specificity,MAX(source.edition_match) AS edition_match,MAX(source.work_match) AS work_match
             FROM candidate_source source
             JOIN candidate_specificity score ON score.query_index=source.query_index AND score.edition_id=source.edition_id AND score.specificity=source.specificity
             GROUP BY source.query_index,source.edition_id,score.specificity
         ),
         eligible_candidate AS (
             SELECT candidate.*
             FROM candidate
             WHERE EXISTS (SELECT 1 FROM edition_classification WHERE edition_id=candidate.edition_id)
                OR EXISTS (
                    SELECT 1
                    FROM edition
                    WHERE edition.edition_id=candidate.edition_id
                      AND (EXISTS (SELECT 1 FROM work_classification WHERE work_id=edition.work_id)
                        OR EXISTS (SELECT 1 FROM edition_work_classification WHERE work_id=edition.work_id))
                )
         ),
         query_best_specificity AS (
             SELECT query_index,MAX(specificity) AS specificity
             FROM eligible_candidate GROUP BY query_index
         ),
         ranked AS (
             SELECT candidate.query_index,candidate.edition_id,candidate.specificity,candidate.edition_match,candidate.work_match,
                    ROW_NUMBER() OVER (PARTITION BY candidate.query_index ORDER BY candidate.edition_id) AS candidate_number
             FROM eligible_candidate candidate
             JOIN query_best_specificity best ON best.query_index=candidate.query_index AND best.specificity=candidate.specificity
         )
         SELECT ranked.query_index,edition.edition_id,edition.work_id,bibliography.title,work_bibliography.title,bibliography.{year_column},ranked.specificity,ranked.edition_match,ranked.work_match
         FROM ranked
         JOIN edition ON edition.edition_id=ranked.edition_id
         LEFT JOIN edition_bibliography bibliography ON bibliography.edition_id=edition.edition_id
         LEFT JOIN work_bibliography ON work_bibliography.work_id=edition.work_id
         WHERE ranked.candidate_number<=? ORDER BY ranked.query_index,ranked.edition_id"
    )
}

// The read-only production snapshot may predate the column rename. Resolve
// only these two known identifiers; no request text is interpolated into SQL.
fn bibliography_year_column(connection: &Connection, table: &str) -> Result<&'static str, MetadataError> {
    let column: String = connection.query_row("SELECT name FROM pragma_table_info(?1) WHERE name IN ('book_year','publication_year') ORDER BY name='book_year' DESC LIMIT 1", [table], |row| row.get(0)).map_err(error)?;
    match column.as_str() {
        "book_year" => Ok("book_year"),
        "publication_year" => Ok("publication_year"),
        _ => unreachable!("query restricts the column names"),
    }
}

fn load_edition_candidates(connection: &Connection, title_rows: &[(usize, String, usize)]) -> Result<BTreeMap<usize, Vec<EditionCandidate>>, MetadataError> {
    if title_rows.is_empty() {
        return Ok(BTreeMap::new());
    }
    let has_subtitles: bool = connection.query_row("SELECT count(*)=2 FROM sqlite_master WHERE type='table' AND name IN ('edition_subtitle','work_subtitle')", [], |row| row.get(0)).map_err(error)?;
    let year_column = bibliography_year_column(connection, "edition_bibliography")?;
    let mut sql = edition_candidate_query(title_rows.len(), year_column);
    if has_subtitles {
        // Add indexed full-title sources alongside the existing main-title
        // sources. Preserve the bounded request order and old snapshot support.
        let edition_start = sql.find("edition_candidate_source(query_index").unwrap();
        let work_start = sql.find("work_candidate_source(query_index").unwrap();
        let combined_start = work_start + 20 + sql[work_start + 20..].find("candidate_source(query_index").unwrap();
        let edition = sql[edition_start..work_start]
            .replace("edition_candidate_source", "edition_subtitle_candidate_source")
            .replace("edition_bibliography AS bibliography INDEXED BY edition_bibliography_by_title", "edition_subtitle AS bibliography INDEXED BY edition_subtitle_by_title")
            .replace("LIMIT ?", &format!("LIMIT {MAX_RAW_TITLE_CANDIDATES}"));
        let work = sql[work_start..combined_start]
            .replace("work_candidate_source", "work_subtitle_candidate_source")
            .replace("work_bibliography AS work_bibliography INDEXED BY work_bibliography_by_title", "work_subtitle AS work_bibliography INDEXED BY work_subtitle_by_title")
            .replace("LIMIT ?", &format!("LIMIT {MAX_RAW_TITLE_CANDIDATES}"));
        sql.insert_str(combined_start, &format!("{edition}{work}"));
        sql = sql.replace("SELECT * FROM edition_candidate_source", "SELECT * FROM edition_subtitle_candidate_source UNION ALL SELECT * FROM work_subtitle_candidate_source UNION ALL SELECT * FROM edition_candidate_source");
        sql = sql.replace("ranked.work_match\n", "ranked.work_match, COALESCE((SELECT subtitle FROM edition_subtitle WHERE edition_id=edition.edition_id), CASE WHEN bibliography.normalized_title=work_bibliography.normalized_title OR bibliography.title IS NULL THEN (SELECT subtitle FROM work_subtitle WHERE work_id=edition.work_id) END) AS subtitle\n");
    } else {
        sql = sql.replace("ranked.work_match\n", "ranked.work_match, NULL AS subtitle\n");
    }
    let mut parameters = Vec::<rusqlite::types::Value>::with_capacity(title_rows.len() * 3 + 3);
    for (query_index, title, specificity) in title_rows {
        parameters.push(i64::try_from(*query_index).map_err(error)?.into());
        parameters.push(title.clone().into());
        parameters.push(i64::try_from(*specificity).map_err(error)?.into());
    }
    parameters.push(i64::try_from(MAX_RAW_TITLE_CANDIDATES).map_err(error)?.into());
    parameters.push(i64::try_from(MAX_RAW_TITLE_CANDIDATES).map_err(error)?.into());
    parameters.push(i64::try_from(MAX_REVERSE_TITLE_CANDIDATES + 1).map_err(error)?.into());
    let mut statement = connection.prepare(&sql).map_err(error)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(parameters), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                EditionCandidate {
                    edition_id: row.get(1)?,
                    work_id: row.get(2)?,
                    edition_title: row.get(3)?,
                    subtitle: row.get(9)?,
                    work_title: row.get(4)?,
                    book_year: row.get(5)?,
                    title_specificity: usize::try_from(row.get::<_, i64>(6)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Integer, Box::new(error)))?,
                    matched_edition_title: row.get(7)?,
                    matched_work_title: row.get(8)?,
                },
            ))
        })
        .map_err(error)?;
    let mut candidates = BTreeMap::<usize, Vec<EditionCandidate>>::new();
    for row in rows {
        let (query_index, candidate) = row.map_err(error)?;
        candidates.entry(usize::try_from(query_index).map_err(error)?).or_default().push(candidate);
    }
    Ok(candidates)
}

fn load_edition_authors(connection: &Connection, edition_ids: &BTreeSet<i64>) -> Result<HashMap<i64, Vec<String>>, MetadataError> {
    load_names_by_ids(connection, edition_ids, "edition_author", "edition_id")
}

fn load_work_authors(connection: &Connection, work_ids: &BTreeSet<i64>) -> Result<HashMap<i64, Vec<String>>, MetadataError> {
    load_names_by_ids(connection, work_ids, "work_author", "work_id")
}

pub(crate) fn load_names_by_ids(connection: &Connection, ids: &BTreeSet<i64>, relation_table: &str, id_column: &str) -> Result<HashMap<i64, Vec<String>>, MetadataError> {
    let mut names = HashMap::<i64, Vec<String>>::new();
    for chunk in ids.iter().copied().collect::<Vec<_>>().chunks(BULK_EVIDENCE_IDS_PER_QUERY) {
        let sql = format!(
            "SELECT reference.{id_column},author.name FROM {relation_table} reference JOIN author ON author.author_id=reference.author_id WHERE reference.{id_column} IN ({}) AND author.name IS NOT NULL ORDER BY reference.{id_column},reference.position",
            sql_placeholders(chunk.len())
        );
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(error)?;
        for row in rows {
            let (id, name) = row.map_err(error)?;
            names.entry(id).or_default().push(name);
        }
    }
    Ok(names)
}

fn load_edition_publishers(connection: &Connection, edition_ids: &BTreeSet<i64>) -> Result<HashMap<i64, Vec<(String, String)>>, MetadataError> {
    let mut publishers = HashMap::<i64, Vec<(String, String)>>::new();
    for chunk in edition_ids.iter().copied().collect::<Vec<_>>().chunks(BULK_EVIDENCE_IDS_PER_QUERY) {
        let sql = format!("SELECT edition_id,name,normalized_name FROM edition_publisher WHERE edition_id IN ({}) ORDER BY edition_id,position", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))).map_err(error)?;
        for row in rows {
            let (edition_id, name, normalized_name) = row.map_err(error)?;
            publishers.entry(edition_id).or_default().push((name, normalized_name));
        }
    }
    Ok(publishers)
}

fn load_edition_isbns(connection: &Connection, edition_ids: &BTreeSet<i64>) -> Result<HashMap<i64, i64>, MetadataError> {
    let mut isbns = HashMap::<i64, i64>::new();
    for chunk in edition_ids.iter().copied().collect::<Vec<_>>().chunks(BULK_EVIDENCE_IDS_PER_QUERY) {
        let sql = format!("SELECT edition_id,isbn13 FROM edition_isbn WHERE edition_id IN ({}) ORDER BY edition_id,isbn13", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).map_err(error)?;
        for row in rows {
            let (edition_id, isbn) = row.map_err(error)?;
            isbns.entry(edition_id).or_insert(isbn);
        }
    }
    Ok(isbns)
}

pub(crate) fn load_classifications_by_ids(connection: &Connection, table: &str, id_column: &str, ids: &BTreeSet<i64>) -> Result<HashMap<i64, Vec<(ClassificationScheme, String)>>, MetadataError> {
    let mut classifications = HashMap::<i64, Vec<(ClassificationScheme, String)>>::new();
    for chunk in ids.iter().copied().collect::<Vec<_>>().chunks(BULK_EVIDENCE_IDS_PER_QUERY) {
        let sql = format!("SELECT {id_column},scheme,notation FROM {table} WHERE {id_column} IN ({}) ORDER BY {id_column},scheme,notation", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))).map_err(error)?;
        for row in rows {
            let (owner_id, scheme, notation) = row.map_err(error)?;
            let scheme = match scheme {
                LCC_SCHEME => ClassificationScheme::LibraryOfCongress,
                _ => continue,
            };
            classifications.entry(owner_id).or_default().push((scheme, notation));
        }
    }
    Ok(classifications)
}

fn resolve_prepared_edition_query(
    connection: &Connection, has_title_author_classification: bool, prepared: &PreparedEditionQuery, candidates: Vec<EditionCandidate>, edition_authors: &HashMap<i64, Vec<String>>, work_authors: &HashMap<i64, Vec<String>>,
    publishers: &HashMap<i64, Vec<(String, String)>>, isbns: &HashMap<i64, i64>, edition_classifications: &HashMap<i64, Vec<(ClassificationScheme, String)>>, work_classifications: &HashMap<i64, Vec<(ClassificationScheme, String)>>,
    sibling_classifications: &HashMap<i64, Vec<(ClassificationScheme, String)>>,
) -> Result<EditionIdentityResult, MetadataError> {
    let mut editions = Vec::<(usize, usize, usize, EditionIdentityMatch)>::new();
    for candidate in candidates {
        let authors = edition_authors.get(&candidate.edition_id).filter(|authors| !authors.is_empty()).or_else(|| candidate.work_id.and_then(|work_id| work_authors.get(&work_id))).cloned().unwrap_or_default();
        let matched_author = metadata_contract::matching::matching_author_count(&prepared.query.authors, &authors);
        if !prepared.author_keys.is_empty() && matched_author == 0 {
            continue;
        }
        let publisher_rows = publishers.get(&candidate.edition_id).cloned().unwrap_or_default();
        let matched_publisher = !prepared.publisher_keys.is_empty() && publisher_rows.iter().any(|(_, key)| prepared.publisher_keys.contains(key));
        let matched_book_year = prepared.query.book_year.is_some() && prepared.query.book_year == candidate.book_year;
        let title_match = match (candidate.matched_edition_title, candidate.matched_work_title) {
            (true, true) => EditionTitleMatch::Both,
            (true, false) => EditionTitleMatch::Edition,
            (false, true) => EditionTitleMatch::Work,
            (false, false) => continue,
        };
        let Some(isbn) = isbns.get(&candidate.edition_id) else { continue };
        let mut classifications = BTreeMap::<(ClassificationScheme, String), ClassificationSource>::new();
        for (scheme, notation) in edition_classifications.get(&candidate.edition_id).into_iter().flatten() {
            classifications.insert((*scheme, notation.clone()), ClassificationSource::ExactEdition);
        }
        if let Some(work_id) = candidate.work_id {
            let exact_schemes = classifications.keys().map(|(scheme, _)| *scheme).collect::<BTreeSet<_>>();
            let mut work_schemes = BTreeSet::new();
            for (scheme, notation) in work_classifications.get(&work_id).into_iter().flatten() {
                if !exact_schemes.contains(scheme) {
                    work_schemes.insert(*scheme);
                    classifications.entry((*scheme, notation.clone())).or_insert(ClassificationSource::Work);
                }
            }
            for (scheme, notation) in sibling_classifications.get(&work_id).into_iter().flatten() {
                if !exact_schemes.contains(scheme) && !work_schemes.contains(scheme) {
                    classifications.entry((*scheme, notation.clone())).or_insert(ClassificationSource::Work);
                }
            }
        }
        if has_title_author_classification && classifications.is_empty() {
            for (scheme, notation) in load_title_author_classifications(connection, prepared, &candidate)? {
                classifications.insert((scheme, notation), ClassificationSource::ExactEdition);
            }
        }
        let classifications =
            metadata_contract::filter_classification_outliers(classifications.into_iter().map(|((scheme, notation), source)| Classification { evidence: Vec::new(), scheme, notation, source }), classification_similarity_keys);
        // Candidate generation requires a usable LCC code. DDC-only records
        // cannot supply subjects through the ISBN-to-LCC enrichment path.
        if !classifications.iter().any(|c| c.scheme == ClassificationScheme::LibraryOfCongress && metadata_contract::classification_consensus_notation(c.scheme, &c.notation).is_some()) {
            continue;
        }
        editions.push((
            candidate.title_specificity,
            matched_author,
            usize::from(matched_publisher) + usize::from(matched_book_year),
            EditionIdentityMatch {
                subtitle: candidate.subtitle,
                canonical_isbn13: format!("{isbn:013}"),
                open_library_edition_id: if candidate.edition_id > 0 { format!("OL{}M", candidate.edition_id) } else { String::new() },
                open_library_work_id: candidate.work_id.map(|work_id| format!("OL{work_id}W")),
                title: candidate.edition_title.clone().unwrap_or_else(|| candidate.work_title.clone().unwrap_or_default()),
                work_title: candidate.work_title,
                title_match,
                authors,
                publishers: publisher_rows.into_iter().map(|(name, _)| name).collect(),
                book_year: candidate.book_year,
                matched_publisher,
                matched_book_year,
                classifications,
            },
        ));
    }
    if editions.len() > 1 {
        let best_specificity = editions.iter().map(|(specificity, _, _, _)| *specificity).max().unwrap_or(0);
        editions.retain(|(specificity, _, _, _)| *specificity == best_specificity);
    }
    if editions.len() > 1 {
        let best_authors = editions.iter().map(|(_, count, _, _)| *count).max().unwrap_or(0);
        editions.retain(|(_, count, _, _)| *count == best_authors);
    }
    if editions.len() > 1 && !prepared.author_keys.is_empty() {
        let best_score = editions.iter().map(|(_, _, score, _)| *score).max().unwrap_or(0);
        if best_score > 0 {
            editions.retain(|(_, _, score, _)| *score == best_score);
        }
    }
    if editions.len() > 1 {
        let mut seen_works = BTreeSet::new();
        editions.retain(|(_, _, _, matched)| matched.open_library_work_id.as_ref().is_none_or(|work_id| seen_works.insert(work_id.clone())));
    }
    // Duplicate records sharing one ISBN and identical codes can collapse.
    // Different ISBNs remain unresolved even when their classifications agree;
    // callers can recover work subjects without inventing an edition identity.
    if editions.len() > 1 && editions.iter().all(|(_, matched_author, _, _)| *matched_author > 0) {
        let classification_codes = |matched: &EditionIdentityMatch| {
            matched.classifications.iter().map(|classification| (classification.scheme, classification.notation.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_uppercase).collect::<String>())).collect::<BTreeSet<_>>()
        };
        let first_codes = classification_codes(&editions[0].3);
        if !first_codes.is_empty()
            && editions.iter().skip(1).all(|(_, _, _, matched)| matched.canonical_isbn13 == editions[0].3.canonical_isbn13 && matched.book_year == editions[0].3.book_year && classification_codes(matched) == first_codes)
        {
            editions.truncate(1);
        }
    }
    // Title agreement identifies candidates, but it is not enough to attach
    // an external edition automatically: generic titles such as "Complete
    // Works" routinely collide across unrelated authors. Publisher and year
    // may rank editions of the same work, while at least one author must agree
    // before the remaining candidate is safe to accept without review.
    let automatic_match = editions.len() == 1 && editions[0].1 > 0;
    let mut strong = editions.into_iter().map(|(_, _, _, matched)| matched).collect::<Vec<_>>();
    Ok(match strong.len() {
        0 => identity_result(&prepared.query.query_id, EditionIdentityStatus::NoMatch, None, Vec::new(), false),
        1 if automatic_match => identity_result(&prepared.query.query_id, EditionIdentityStatus::Matched, strong.pop(), Vec::new(), false),
        _ => {
            let candidates_truncated = strong.len() > MAX_EDITION_IDENTITY_REVIEW_CANDIDATES;
            strong.truncate(MAX_EDITION_IDENTITY_REVIEW_CANDIDATES);
            identity_result(&prepared.query.query_id, EditionIdentityStatus::Ambiguous, None, strong, candidates_truncated)
        }
    })
}

fn load_title_author_classifications(connection: &Connection, prepared: &PreparedEditionQuery, candidate: &EditionCandidate) -> Result<Vec<(ClassificationScheme, String)>, MetadataError> {
    if prepared.author_keys.is_empty() {
        return Ok(Vec::new());
    }
    let year_column = bibliography_year_column(connection, "title_author_classification")?;
    let sql = format!("SELECT normalized_author,{year_column},normalized_publisher,scheme,notation FROM title_author_classification WHERE normalized_title=?1 AND normalized_subtitle=?2 ORDER BY scheme,notation LIMIT 129");
    let mut values = Vec::<rusqlite::types::Value>::with_capacity(prepared.author_keys.len() + 2);
    values.push(metadata_contract::matching::bibliographic_title_match_key(candidate.edition_title.as_deref().or(candidate.work_title.as_deref()).unwrap_or_default()).into());
    values.push(candidate.subtitle.as_deref().map(|value| value.trim().to_lowercase()).unwrap_or_default().into());
    let mut statement = connection.prepare(&sql).map_err(error)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, String>(4)?)))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    if rows.len() > 128 {
        return Ok(Vec::new());
    }
    let mut best = BTreeMap::<(ClassificationScheme, String), usize>::new();
    for row in rows {
        let (author, year, publisher, scheme, notation) = row;
        if !prepared.query.authors.iter().any(|local| metadata_contract::matching::author_names_compatible(local, &author)) {
            continue;
        }
        let scheme = match scheme {
            1 => ClassificationScheme::DeweyDecimal,
            LCC_SCHEME => ClassificationScheme::LibraryOfCongress,
            _ => continue,
        };
        let score = usize::from(prepared.query.book_year.is_some_and(|value| value == i32::try_from(year).unwrap_or_default() && year != 0)) + usize::from(!publisher.is_empty() && prepared.publisher_keys.contains(&publisher));
        best.entry((scheme, notation)).and_modify(|current| *current = (*current).max(score)).or_insert(score);
    }
    Ok(best.into_keys().collect())
}

pub(crate) fn identity_result(query_id: &str, status: EditionIdentityStatus, matched: Option<EditionIdentityMatch>, candidates: Vec<EditionIdentityMatch>, candidates_truncated: bool) -> EditionIdentityResult {
    EditionIdentityResult { authority_subjects: None, query_id: query_id.to_owned(), status, matched, candidates, candidates_truncated }
}

fn identity_title_keys(title: &str) -> Vec<(String, usize)> {
    metadata_contract::matching::NormalizedTitle::new(title).candidate_keys(super::MAX_TITLE_SUBSTRINGS_PER_QUERY)
}

fn title_qualifiers(title: &str) -> BTreeSet<String> {
    metadata_contract::matching::title_qualifiers(title)
}
