//! Export derived LCC browse ranks for viewer/runtime parity audits.
use std::path::Path;
use subject_projection::{lcc_subject_sort_orders, read_unified_taxonomy_sqlite};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: taxonomy_order TAXONOMY.sqlite3")?;
    let definitions = read_unified_taxonomy_sqlite(Path::new(&path))?;
    let mut output = csv::Writer::from_writer(std::io::stdout());
    output.write_record(["concept_id", "derived_order"])?;
    for (id, rank) in lcc_subject_sort_orders(&definitions) {
        output.write_record([id.to_string(), rank.to_string()])?;
    }
    output.flush()?;
    Ok(())
}
