//! Inspect production matcher destinations for explicit classification probes.
use std::path::Path;
use subject_projection::{read_unified_taxonomy_sqlite, UnifiedConceptDefinition, UnifiedTaxonomy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() < 3 {
        return Err("usage: taxonomy_probe TAXONOMY.sqlite3 [--code-only] SYSTEM:CODE...".into());
    }
    let code_only = args[2] == "--code-only";
    let mut definitions = read_unified_taxonomy_sqlite(Path::new(&args[1]))?;
    if code_only {
        // Isolate code matching from pre-existing display-route label conflicts.
        // IDs, parent edges and every selector remain identical to the database.
        definitions = definitions
            .iter()
            .map(|c| {
                UnifiedConceptDefinition::new(
                    c.concept_id(),
                    format!("Subject {}", c.concept_id()),
                    c.parent_ids().to_vec(),
                    ["lcc", "bisac", "ddc"].into_iter().map(|s| (s.to_owned(), c.source_selectors(s).to_vec())).collect(),
                )
            })
            .collect();
    }
    let taxonomy = UnifiedTaxonomy::from_concepts(definitions)?;
    let mut output = csv::Writer::from_writer(std::io::stdout());
    output.write_record(["system", "code", "resolved", "candidates"])?;
    for probe in &args[if code_only { 3 } else { 2 }..] {
        let (system, code) = probe.split_once(':').ok_or("expected SYSTEM:CODE")?;
        let ids = |values: Vec<i64>| values.iter().map(i64::to_string).collect::<Vec<_>>().join("|");
        output.write_record([system, code, &ids(taxonomy.matching_concept_ids(system, code)), &ids(taxonomy.match_candidates(system, code))])?;
    }
    output.flush()?;
    Ok(())
}
