//! Print an audited pruning proposal; never changes the input database.
use std::path::Path;
use subject_projection::{read_unified_taxonomy_sqlite, redundant_lcc_exact_selectors};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: prune_lcc_selectors TAXONOMY.sqlite3")?;
    let definitions = read_unified_taxonomy_sqlite(Path::new(&path))?;
    let removed = redundant_lcc_exact_selectors(&definitions)?;
    eprintln!("{} exact LCC selectors are redundant with existing ranges or broader exact codes", removed.len());
    let mut output = csv::Writer::from_writer(std::io::stdout());
    output.write_record(["concept_id", "selector"])?;
    for (id, selector) in removed {
        output.write_record([id.to_string(), selector])?;
    }
    output.flush()?;
    Ok(())
}
