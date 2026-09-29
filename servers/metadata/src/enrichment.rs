use super::{
    canonical_isbn13, error, open_library_id, AuthorIdentifier, AuthorMetadata, AuthorSource, BTreeMap, BTreeSet, Classification, ClassificationScheme, ClassificationSource, Connection, IsbnClassificationResult, IsbnMetadataResult,
    LookupStatus, MetadataError, RichIsbnMetadataResult, RichWorkMetadataMatch, WorkClassificationMatch, LCC_SCHEME,
};

const MAX_DIRECT_WORK_CLASSIFICATIONS_WITHOUT_CONSENSUS: usize = 12;

pub(crate) fn lookup_one(connection: &Connection, requested_isbn: String) -> Result<IsbnClassificationResult, MetadataError> {
    let Some(isbn13) = canonical_isbn13(&requested_isbn) else {
        return Ok(IsbnClassificationResult { requested_isbn, canonical_isbn13: None, status: LookupStatus::InvalidIsbn, matches: Vec::new() });
    };
    let mut editions = connection
        .prepare_cached("SELECT edition.edition_id,edition.work_id FROM edition_isbn JOIN edition ON edition.edition_id=edition_isbn.edition_id WHERE edition_isbn.isbn13=?1 ORDER BY edition.work_id,edition.edition_id")
        .map_err(error)?;
    let rows = editions.query_map([isbn13], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))).map_err(error)?;
    let mut by_work = BTreeMap::<(Option<i64>, i64), Vec<i64>>::new();
    for row in rows {
        let (edition_id, work_id) = row.map_err(error)?;
        by_work.entry((work_id, work_id.unwrap_or(edition_id))).or_default().push(edition_id);
    }
    let mut matches = Vec::with_capacity(by_work.len());
    for ((work_id, _group_id), edition_ids) in by_work {
        let mut classifications = BTreeMap::<(ClassificationScheme, String), ClassificationSource>::new();
        for edition_id in &edition_ids {
            collect_classifications(connection, "edition_classification", "edition_id", *edition_id, ClassificationSource::ExactEdition, &mut classifications)?;
        }
        if let Some(work_id) = work_id {
            let exact_schemes = classifications.keys().map(|(scheme, _)| *scheme).collect::<BTreeSet<_>>();
            let mut imported_work_classifications = BTreeMap::new();
            collect_classifications(connection, "work_classification", "work_id", work_id, ClassificationSource::Work, &mut imported_work_classifications)?;
            // Open Library's work record is the authority signal that makes
            // its edition-level union useful. Without it, require independent
            // sibling consensus so one noisy edition cannot classify a work.
            if !imported_work_classifications.is_empty() && imported_work_classifications.len() <= MAX_DIRECT_WORK_CLASSIFICATIONS_WITHOUT_CONSENSUS {
                let work_schemes = imported_work_classifications.keys().map(|(scheme, _)| *scheme).collect::<BTreeSet<_>>();
                for (classification, source) in imported_work_classifications {
                    if !exact_schemes.contains(&classification.0) {
                        classifications.entry(classification).or_insert(source);
                    }
                }
                let mut sibling_classifications = BTreeMap::new();
                collect_classifications(connection, "edition_work_classification", "work_id", work_id, ClassificationSource::Work, &mut sibling_classifications)?;
                for (classification, source) in sibling_classifications {
                    if !exact_schemes.contains(&classification.0) && !work_schemes.contains(&classification.0) {
                        classifications.entry(classification).or_insert(source);
                    }
                }
            }
            for classification in collect_sibling_edition_consensus(connection, work_id)? {
                if !exact_schemes.contains(&classification.scheme) && !classifications.keys().any(|(scheme, _)| *scheme == classification.scheme) {
                    classifications.entry((classification.scheme, classification.notation)).or_insert(ClassificationSource::Work);
                }
            }
        }
        let classifications =
            metadata_contract::filter_classification_outliers(classifications.into_iter().map(|((scheme, notation), source)| Classification { evidence: Vec::new(), scheme, notation, source }), classification_similarity_keys);
        matches.push(WorkClassificationMatch {
            open_library_work_id: work_id.map(|work_id| format!("OL{work_id}W")),
            exact_edition_ids: edition_ids.into_iter().map(|edition_id| crate::openlibrary::authority_edition_key(edition_id)).collect(),
            classifications,
        });
    }
    // Source ingestion writes the canonical ISBN classification table. Older
    // snapshots may predate it; existing edition/work evidence remains valid.
    let has_isbn_codes: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='isbn_classification')", [], |r| r.get(0)).map_err(error)?;
    if has_isbn_codes {
        let mut direct = BTreeMap::<(ClassificationScheme, String), ClassificationSource>::new();
        let mut statement = connection.prepare_cached("SELECT scheme,notation FROM isbn_classification WHERE isbn13=?1 ORDER BY scheme,notation").map_err(error)?;
        let rows = statement.query_map([isbn13], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(error)?;
        for row in rows {
            let (scheme, notation) = row.map_err(error)?;
            let scheme = match scheme {
                1 => ClassificationScheme::DeweyDecimal,
                LCC_SCHEME => ClassificationScheme::LibraryOfCongress,
                _ => continue,
            };
            direct.insert((scheme, notation), ClassificationSource::ExactEdition);
        }
        if !direct.is_empty() {
            if matches.is_empty() {
                let classifications = direct.into_iter().map(|((scheme, notation), source)| Classification { evidence: Vec::new(), scheme, notation, source }).collect();
                matches.push(WorkClassificationMatch { open_library_work_id: None, exact_edition_ids: Vec::new(), classifications });
            } else {
                for matched in &mut matches {
                    let mut merged = matched.classifications.iter().map(|c| ((c.scheme, c.notation.clone()), c.source)).collect::<BTreeMap<_, _>>();
                    for (classification, source) in &direct {
                        merged.entry(classification.clone()).or_insert(*source);
                    }
                    matched.classifications =
                        metadata_contract::filter_classification_outliers(merged.into_iter().map(|((scheme, notation), source)| Classification { evidence: Vec::new(), scheme, notation, source }), classification_similarity_keys);
                }
            }
        }
    }
    crate::loc_dump::merge_lcc(&mut matches, crate::loc_dump::lookup_lcc(connection, isbn13)?);
    let status = match matches.len() {
        0 => LookupStatus::NoMatch,
        1 => LookupStatus::Matched,
        _ => LookupStatus::Ambiguous,
    };
    Ok(IsbnClassificationResult { requested_isbn, canonical_isbn13: Some(format!("{isbn13:013}")), status, matches })
}

pub(crate) fn attach_rich_fields(connection: &Connection, metadata: Vec<IsbnMetadataResult>, descriptions: &BTreeMap<i64, String>, bisac: &BTreeMap<i64, Vec<Classification>>) -> Result<Vec<RichIsbnMetadataResult>, MetadataError> {
    let edition_ids = metadata.iter().flat_map(|result| &result.matches).flat_map(|matched| &matched.exact_edition_ids).filter_map(|value| crate::openlibrary::authority_edition_id(value)).collect::<BTreeSet<_>>();
    let mut edition_descriptions = BTreeMap::<i64, String>::new();
    let has: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='edition_description')", [], |r| r.get(0)).map_err(error)?;
    if has {
        for ids in edition_ids.iter().copied().collect::<Vec<_>>().chunks(400) {
            let sql = format!("SELECT edition_id,description FROM edition_description WHERE edition_id IN ({}) AND description IS NOT NULL AND trim(description)!=''", sql_placeholders(ids.len()));
            let mut q = connection.prepare(&sql).map_err(error)?;
            for row in q.query_map(rusqlite::params_from_iter(ids), |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)? {
                let (id, d) = row.map_err(error)?;
                edition_descriptions.insert(id, d);
            }
        }
    }

    metadata
        .into_iter()
        .map(|result| {
            let matches = result
                .matches
                .into_iter()
                .map(|matched| {
                    let work_id = matched.open_library_work_id.as_deref().and_then(|value| open_library_id(value, 'W'));
                    let mut classifications = matched.classifications;
                    classifications.extend(work_id.and_then(|work_id| bisac.get(&work_id)).into_iter().flatten().cloned());
                    let description = work_id.and_then(|id| descriptions.get(&id).cloned()).or_else(|| {
                        let values = matched.exact_edition_ids.iter().filter_map(|id| crate::openlibrary::authority_edition_id(id)).filter_map(|id| edition_descriptions.get(&id)).collect::<BTreeSet<_>>();
                        (values.len() == 1).then(|| (*values.iter().next().unwrap()).clone())
                    });
                    RichWorkMetadataMatch { open_library_work_id: matched.open_library_work_id, exact_edition_ids: matched.exact_edition_ids, classifications, authors: matched.authors, description, subjects: Vec::new() }
                })
                .collect();
            Ok(RichIsbnMetadataResult { wikidata_books: Vec::new(), requested_isbn: result.requested_isbn, canonical_isbn13: result.canonical_isbn13, status: result.status, matches })
        })
        .collect()
}

pub(crate) fn load_embedded_work_descriptions(connection: &Connection, ids: &BTreeSet<i64>) -> Result<BTreeMap<i64, String>, MetadataError> {
    let table_exists = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_description')", (), |row| row.get::<_, bool>(0)).map_err(error)?;
    if !table_exists {
        return Ok(BTreeMap::new());
    }
    load_work_descriptions_from_table(connection, ids)
}

pub(crate) fn load_work_descriptions_from_table(connection: &Connection, ids: &BTreeSet<i64>) -> Result<BTreeMap<i64, String>, MetadataError> {
    let mut descriptions = BTreeMap::new();
    for chunk in ids.iter().copied().collect::<Vec<_>>().chunks(500) {
        let sql = format!("SELECT work_id,description FROM work_description WHERE work_id IN ({})", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(error)?;
        for row in rows {
            let (work_id, description) = row.map_err(error)?;
            descriptions.insert(work_id, description);
        }
    }
    Ok(descriptions)
}

pub(crate) fn load_work_bisac_classifications(connection: &Connection, ids: &BTreeSet<i64>) -> Result<BTreeMap<i64, Vec<Classification>>, MetadataError> {
    let mut classifications = BTreeMap::<i64, Vec<Classification>>::new();
    for chunk in ids.iter().copied().collect::<Vec<_>>().chunks(500) {
        let sql = format!("SELECT DISTINCT work_id,code FROM work_bisac_subject WHERE work_id IN ({}) ORDER BY work_id,code", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(error)?;
        for row in rows {
            let (work_id, notation) = row.map_err(error)?;
            classifications.entry(work_id).or_default().push(Classification { evidence: Vec::new(), scheme: ClassificationScheme::Bisac, notation, source: ClassificationSource::Work });
        }
    }
    Ok(classifications)
}

pub(crate) fn sql_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count).collect::<Vec<_>>().join(",")
}

pub(crate) fn attach_authors(connection: &Connection, results: &mut [IsbnMetadataResult]) -> Result<(), MetadataError> {
    let edition_ids = results.iter().flat_map(|result| &result.matches).flat_map(|matched| &matched.exact_edition_ids).filter_map(|value| crate::openlibrary::authority_edition_id(value)).collect::<BTreeSet<_>>();
    let work_ids = results.iter().flat_map(|result| &result.matches).filter_map(|matched| matched.open_library_work_id.as_deref().and_then(|value| open_library_id(value, 'W'))).collect::<BTreeSet<_>>();
    let edition_authors = load_author_relations(connection, "edition_author", "edition_id", &edition_ids)?;
    let work_authors = load_author_relations(connection, "work_author", "work_id", &work_ids)?;
    let author_ids = edition_authors.values().chain(work_authors.values()).flatten().map(|(author_id, _)| *author_id).collect::<BTreeSet<_>>();
    let names = load_author_names(connection, &author_ids)?;
    let identifiers = load_author_identifiers(connection, &author_ids)?;

    for matched in results.iter_mut().flat_map(|result| &mut result.matches) {
        let mut authors = BTreeMap::<i64, (AuthorSource, u32)>::new();
        for edition_id in matched.exact_edition_ids.iter().filter_map(|value| crate::openlibrary::authority_edition_id(value)) {
            merge_author_relations(&mut authors, edition_authors.get(&edition_id), AuthorSource::ExactEdition);
        }
        if let Some(work_id) = matched.open_library_work_id.as_deref().and_then(|value| open_library_id(value, 'W')) {
            merge_author_relations(&mut authors, work_authors.get(&work_id), AuthorSource::Work);
        }
        matched.authors = authors
            .into_iter()
            .map(|(author_id, (source, position))| AuthorMetadata {
                open_library_author_id: if author_id > 0 { format!("OL{author_id}A") } else { String::new() },
                name: names.get(&author_id).cloned().flatten(),
                identifiers: identifiers.get(&author_id).cloned().unwrap_or_default(),
                source,
                position,
            })
            .collect();
        matched.authors.sort_by_key(|author| (author.source, author.position, author.open_library_author_id.clone()));
    }
    Ok(())
}

pub(crate) fn load_author_relations(connection: &Connection, table: &str, id_column: &str, ids: &BTreeSet<i64>) -> Result<BTreeMap<i64, Vec<(i64, u32)>>, MetadataError> {
    let mut relations = BTreeMap::<i64, Vec<(i64, u32)>>::new();
    for chunk in ids.iter().copied().collect::<Vec<_>>().chunks(500) {
        let sql = format!("SELECT {id_column},author_id,position FROM {table} WHERE {id_column} IN ({}) ORDER BY {id_column},position", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))).map_err(error)?;
        for row in rows {
            let (owner_id, author_id, position) = row.map_err(error)?;
            relations.entry(owner_id).or_default().push((author_id, u32::try_from(position).map_err(error)?));
        }
    }
    Ok(relations)
}

pub(crate) fn merge_author_relations(authors: &mut BTreeMap<i64, (AuthorSource, u32)>, relations: Option<&Vec<(i64, u32)>>, source: AuthorSource) {
    for &(author_id, position) in relations.into_iter().flatten() {
        authors
            .entry(author_id)
            .and_modify(|existing| {
                if (source, position) < *existing {
                    *existing = (source, position);
                }
            })
            .or_insert((source, position));
    }
}

pub(crate) fn load_author_names(connection: &Connection, ids: &BTreeSet<i64>) -> Result<BTreeMap<i64, Option<String>>, MetadataError> {
    let mut names = BTreeMap::new();
    for chunk in ids.iter().copied().collect::<Vec<_>>().chunks(500) {
        let sql = format!("SELECT author_id,name FROM author WHERE author_id IN ({})", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))).map_err(error)?;
        for row in rows {
            let (author_id, name) = row.map_err(error)?;
            names.insert(author_id, name);
        }
    }
    Ok(names)
}

pub(crate) fn load_author_identifiers(connection: &Connection, ids: &BTreeSet<i64>) -> Result<BTreeMap<i64, Vec<AuthorIdentifier>>, MetadataError> {
    let mut identifiers = BTreeMap::<i64, Vec<AuthorIdentifier>>::new();
    for chunk in ids.iter().copied().collect::<Vec<_>>().chunks(500) {
        let sql = format!("SELECT author_id,authority,external_id FROM author_identifier WHERE author_id IN ({}) ORDER BY author_id,authority,external_id", sql_placeholders(chunk.len()));
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok((row.get::<_, i64>(0)?, AuthorIdentifier { authority: row.get(1)?, value: row.get(2)? }))).map_err(error)?;
        for row in rows {
            let (author_id, identifier) = row.map_err(error)?;
            identifiers.entry(author_id).or_default().push(identifier);
        }
    }
    Ok(identifiers)
}

fn collect_classifications(connection: &Connection, table: &str, id_column: &str, id: i64, source: ClassificationSource, classifications: &mut BTreeMap<(ClassificationScheme, String), ClassificationSource>) -> Result<(), MetadataError> {
    let sql = format!("SELECT scheme,notation FROM {table} WHERE {id_column}=?1 ORDER BY scheme,notation");
    let mut statement = connection.prepare_cached(&sql).map_err(error)?;
    let rows = statement.query_map([id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(error)?;
    for row in rows {
        let (scheme, notation) = row.map_err(error)?;
        let scheme = match scheme {
            LCC_SCHEME => ClassificationScheme::LibraryOfCongress,
            _ => continue,
        };
        classifications.entry((scheme, notation)).and_modify(|existing| *existing = (*existing).min(source)).or_insert(source);
    }
    Ok(())
}

fn collect_sibling_edition_consensus(connection: &Connection, work_id: i64) -> Result<Vec<Classification>, MetadataError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT edition_classification.edition_id,edition_classification.scheme,edition_classification.notation
             FROM edition_classification
             JOIN edition ON edition.edition_id=edition_classification.edition_id
             WHERE edition.work_id=?1
             ORDER BY edition_classification.edition_id,edition_classification.scheme,edition_classification.notation",
        )
        .map_err(error)?;
    let rows = statement.query_map([work_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))).map_err(error)?;
    let mut support = BTreeMap::<(ClassificationScheme, String), BTreeSet<i64>>::new();
    let mut classified_editions = BTreeSet::new();
    for row in rows {
        let (edition_id, scheme, notation) = row.map_err(error)?;
        let scheme = match scheme {
            LCC_SCHEME => ClassificationScheme::LibraryOfCongress,
            _ => continue,
        };
        let Some(notation) = metadata_contract::classification_consensus_notation(scheme, &notation) else { continue };
        classified_editions.insert(edition_id);
        support.entry((scheme, notation)).or_default().insert(edition_id);
    }
    Ok(support
        .into_iter()
        .filter_map(|((scheme, notation), editions)| (classified_editions.len() == 1 || editions.len() >= 2).then_some(Classification { evidence: Vec::new(), scheme, notation, source: ClassificationSource::Work }))
        .collect())
}

pub(crate) fn classification_similarity_keys(scheme: ClassificationScheme, notation: &str) -> Vec<String> {
    let system_id = match scheme {
        ClassificationScheme::LibraryOfCongress => subject_projection::LCC_SYSTEM_ID,
        ClassificationScheme::DeweyDecimal => return Vec::new(),
        ClassificationScheme::Bisac => subject_projection::BISAC_SYSTEM_ID,
    };
    subject_projection::unified_subject_similarity_keys(system_id, notation)
}
