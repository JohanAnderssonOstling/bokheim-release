//! Compare frozen taxonomies using full production matching, without route-label
//! collisions elsewhere preventing a code audit. Input: system<TAB>uses<TAB>hex.
use std::{
    collections::BTreeMap,
    io::{self, BufRead, Write},
    path::Path,
};
use subject_projection::{read_unified_taxonomy_sqlite, UnifiedConceptDefinition, UnifiedTaxonomy};

fn load(path: &str) -> Result<UnifiedTaxonomy, Box<dyn std::error::Error>> {
    let definitions = read_unified_taxonomy_sqlite(Path::new(path))?
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
    Ok(UnifiedTaxonomy::from_concepts(definitions)?)
}
fn ids(v: &[i64]) -> String {
    v.iter().map(i64::to_string).collect::<Vec<_>>().join("|")
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let before = load(&args[1])?;
    let after = load(&args[2])?;
    let mut out = io::BufWriter::new(io::stdout().lock());
    let mut counts = BTreeMap::<i64, u64>::new();
    let mut totals = [0u64; 4];
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields = line.splitn(3, '\t').collect::<Vec<_>>();
        let uses = fields[1].parse::<u64>()?;
        let bytes = (0..fields[2].len()).step_by(2).map(|i| u8::from_str_radix(&fields[2][i..i + 2], 16)).collect::<Result<Vec<_>, _>>()?;
        let code = String::from_utf8(bytes)?;
        let old = before.matching_concept_ids(fields[0], &code);
        let new = after.matching_concept_ids(fields[0], &code);
        totals[0] += 1;
        if old != new {
            totals[1] += 1;
            if !old.is_empty() && new.is_empty() {
                totals[2] += 1;
            }
            if old.is_empty() && !new.is_empty() {
                totals[3] += 1;
            }
            writeln!(out, "{}\t{}\t{}\t{}\t{}", fields[0], uses, fields[2], ids(&old), ids(&new))?;
        }
        if fields[0] == "lcc" {
            for id in new {
                *counts.entry(id).or_default() += uses;
            }
        }
        if totals[0] % 100000 == 0 {
            eprintln!("Processed {}", totals[0]);
        }
    }
    out.flush()?;
    let mut counts_out = io::BufWriter::new(std::fs::File::create(&args[3])?);
    for (id, count) in counts {
        writeln!(counts_out, "{id}\t{count}")?;
    }
    eprintln!("Probes={} changed={} losses={} gains={}", totals[0], totals[1], totals[2], totals[3]);
    Ok(())
}
