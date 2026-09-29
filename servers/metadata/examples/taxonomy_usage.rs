use rusqlite::Connection;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = std::env::args().nth(1).expect("usage: taxonomy_usage DATABASE.sqlite");
    let taxonomy_database = std::env::args().nth(2).expect("usage: taxonomy_usage DATABASE.sqlite TAXONOMY.sqlite3");
    let output = std::env::args().nth(3);
    let match_output = std::env::args().nth(4);
    let concepts = subject_projection::read_unified_taxonomy_sqlite(Path::new(&taxonomy_database))?;
    subject_projection::install_unified_taxonomy_concepts(concepts)?;
    let connection = Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-32768")?;
    let has_compact_usage = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='code_usage')", [], |row| row.get::<_, bool>(0))?;
    if has_compact_usage {
        let has_metadata = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='cache_metadata')", [], |row| row.get::<_, bool>(0))?;
        if has_metadata {
            let full_notation = connection.query_row("SELECT EXISTS(SELECT 1 FROM cache_metadata WHERE key='normalization_version' AND value='2')", [], |row| row.get::<_, bool>(0))?;
            if !full_notation {
                return Err("classification cache uses obsolete truncating normalization; rebuild it with rebuild-code-usage.py before refreshing taxonomy usage".into());
            }
        }
    }
    let usage_query = if has_compact_usage {
        "SELECT scheme,notation,uses FROM code_usage ORDER BY uses DESC,scheme,notation"
    } else {
        "WITH raw(scheme,notation) AS (
             SELECT scheme,notation FROM edition_classification
             UNION ALL SELECT scheme,notation FROM work_classification
         ), compact(scheme,notation) AS (
             SELECT scheme,trim(notation) FROM raw
         )
         SELECT scheme,notation,COUNT(*) AS uses
         FROM compact GROUP BY scheme,notation ORDER BY uses DESC,scheme,notation"
    };
    let mut statement = connection.prepare(usage_query)?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?;
    let mut counts = HashMap::<String, i64>::new();
    let mut contributions = HashMap::<String, Vec<(String, i64)>>::new();
    let concept_by_path = subject_projection::unified_taxonomy_routes().into_iter().map(|route| (route.path().to_owned(), route.concept_id().to_owned())).collect::<HashMap<_, _>>();
    // A complete cache can contain millions of codes: stream match rows rather
    // than retaining a second copy of every matched path in memory.
    let mut match_writer = match_output.map(std::fs::File::create).transpose()?.map(|file| csv::WriterBuilder::new().delimiter(b'\t').from_writer(std::io::BufWriter::new(file)));
    if let Some(file) = &mut match_writer {
        file.write_record(["scheme", "notation", "uses", "concept_id", "path"])?;
    }
    let mut sampled = 0_i64;
    let mut matched = 0_i64;
    for row in rows {
        let (scheme, notation, uses) = row?;
        let system = match scheme {
            2 => subject_projection::LCC_SYSTEM_ID,
            _ => continue,
        };
        sampled += uses;
        let paths = subject_projection::unified_subject_paths(system, &notation);
        if !paths.is_empty() {
            matched += uses;
        }
        for path in paths {
            if let Some(file) = &mut match_writer {
                if let Some(concept_id) = concept_by_path.get(&path) {
                    file.write_record([&scheme.to_string(), &notation, &uses.to_string(), &concept_id.to_string(), &path])?;
                }
            }
            *counts.entry(path.clone()).or_default() += uses;
            // Input is already ranked by uses and notation. Keep only the
            // diagnostic top 20, not millions of full call numbers per branch.
            let top = contributions.entry(path).or_default();
            if top.len() < 20 {
                top.push((notation.clone(), uses));
            }
        }
    }
    let taxonomy = Connection::open_with_flags(taxonomy_database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let selector_counts = taxonomy
        .prepare(
            "SELECT concept_id,SUM(selector_count) FROM (
                 SELECT concept_id,COUNT(*) selector_count FROM lcc_selector GROUP BY concept_id
                 UNION ALL
                 SELECT concept_id,COUNT(*) selector_count FROM lcc_range GROUP BY concept_id
             ) GROUP BY concept_id",
        )?
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<Result<HashMap<_, _>, _>>()?;
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    if let Some(output) = output {
        let mut file = std::io::BufWriter::new(std::fs::File::create(output)?);
        writeln!(file, "concept_id\tdirect_hits\tpath")?;
        for (path, count) in &ranked {
            if let Some(concept_id) = concept_by_path.get(path) {
                writeln!(file, "{concept_id}\t{count}\t{path}")?;
            }
        }
    }
    if let Some(file) = &mut match_writer {
        file.flush()?;
    }
    println!("SUMMARY\t{sampled}\t{matched}");
    for (path, count) in &ranked {
        if path.ends_with("Public Administration & Local Government") {
            println!("TARGET\t{count}\t{path}");
        }
        if path == "Humanities / History / History by Region / Europe / Southern Europe / Italy" {
            let mut sources = contributions.get(path).cloned().unwrap_or_default();
            sources.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            let top = sources.into_iter().take(20).map(|(code, uses)| format!("{code}={uses}")).collect::<Vec<_>>().join(",");
            println!("TARGET_ITALY\t{count}\t{top}");
        }
    }
    for (path, count) in ranked.into_iter().take(50) {
        let selectors = concept_by_path.get(&path).and_then(|id| selector_counts.get(id)).copied().unwrap_or_default();
        let mut sources = contributions.remove(&path).unwrap_or_default();
        sources.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let top = sources.into_iter().take(8).map(|(code, uses)| format!("{code}={uses}")).collect::<Vec<_>>().join(",");
        println!("HIT\t{count}\t{selectors}\t{path}\t{top}");
    }
    Ok(())
}
