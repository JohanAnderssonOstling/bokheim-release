//! Report used LCC notations whose resolved concept set changes between two taxonomies.

use std::path::Path;

use subject_projection::{read_unified_taxonomy_sqlite, UnifiedTaxonomy, LCC_SYSTEM_ID};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: lcc_compare_taxonomies BEFORE.sqlite3 AFTER.sqlite3 CODE_USAGE.sqlite3".into());
    }
    let before = UnifiedTaxonomy::from_concepts(read_unified_taxonomy_sqlite(Path::new(&args[1]))?)?;
    let after = UnifiedTaxonomy::from_concepts(read_unified_taxonomy_sqlite(Path::new(&args[2]))?)?;
    let db = rusqlite::Connection::open_with_flags(&args[3], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut query = db.prepare("SELECT notation,uses FROM code_usage WHERE scheme=2 ORDER BY uses DESC")?;
    let mut output = csv::WriterBuilder::new().delimiter(b'\t').from_writer(std::io::stdout());
    output.write_record(["uses", "notation", "before", "after"])?;
    for row in query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))? {
        let (notation, uses) = row?;
        let old = before.matching_concept_ids(LCC_SYSTEM_ID, &notation);
        let new = after.matching_concept_ids(LCC_SYSTEM_ID, &notation);
        if old != new {
            output.write_record([uses.to_string(), notation, old.iter().map(i64::to_string).collect::<Vec<_>>().join("|"), new.iter().map(i64::to_string).collect::<Vec<_>>().join("|")])?;
        }
    }
    output.flush()?;
    Ok(())
}
