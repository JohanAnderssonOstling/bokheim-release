//! List the most-used LCC notations whose resolved subject is one of the requested concepts.

use std::{collections::HashSet, path::Path};

use subject_projection::{read_unified_taxonomy_sqlite, UnifiedTaxonomy, LCC_SYSTEM_ID};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() < 4 {
        return Err("usage: lcc_direct_codes TAXONOMY.sqlite3 CODE_USAGE.sqlite3 CONCEPT_ID...".into());
    }
    let taxonomy = UnifiedTaxonomy::from_concepts(read_unified_taxonomy_sqlite(Path::new(&args[1]))?)?;
    let targets = args[3..].iter().map(|value| value.parse::<i64>()).collect::<Result<HashSet<_>, _>>()?;
    let db = rusqlite::Connection::open_with_flags(&args[2], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut query = db.prepare("SELECT notation,uses FROM code_usage WHERE scheme=2 ORDER BY uses DESC")?;
    let mut output = csv::WriterBuilder::new().delimiter(b'\t').from_writer(std::io::stdout());
    output.write_record(["concept_id", "uses", "notation"])?;
    for row in query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))? {
        let (notation, uses) = row?;
        let matches = taxonomy.matching_concept_ids(LCC_SYSTEM_ID, &notation);
        if matches.len() == 1 && targets.contains(&matches[0]) {
            output.write_record([matches[0].to_string(), uses.to_string(), notation])?;
        }
    }
    output.flush()?;
    Ok(())
}
