//! Read-only coverage audit of master History codes and observed D/E/F codes.
use std::path::Path;
use subject_projection::{canonical_lcc_notation, read_unified_taxonomy_sqlite, UnifiedTaxonomy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: history_coverage CURATED MASTER CODE_USAGE".into());
    }
    let taxonomy = UnifiedTaxonomy::from_concepts(read_unified_taxonomy_sqlite(Path::new(&args[1]))?)?;
    let master = rusqlite::Connection::open_with_flags(&args[2], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut reference = master.prepare("WITH RECURSIVE t(id) AS (SELECT 3319 UNION SELECT cp.concept_id FROM concept_parent cp JOIN t ON cp.parent_concept_id=t.id) SELECT DISTINCT system_id,selector FROM source_selector JOIN t ON id=concept_id ORDER BY system_id,selector")?;
    let mut output = csv::Writer::from_writer(std::io::stdout());
    output.write_record(["source", "system", "code", "uses", "status", "candidates"])?;
    let mut counts = std::collections::BTreeMap::<String, (u64, i64)>::new();
    let mut check = |source: &str, system: &str, code: &str, uses: i64| -> Result<(), Box<dyn std::error::Error>> {
        let resolved = taxonomy.matching_concept_ids(system, code);
        let candidates = if resolved.is_empty() { taxonomy.match_candidates(system, code) } else { Vec::new() };
        let status = if !resolved.is_empty() {
            "covered"
        } else if candidates.len() > 1 {
            "ambiguous"
        } else if system == "lcc" && canonical_lcc_notation(code).is_none() {
            "unsupported_syntax"
        } else {
            "uncovered"
        };
        let entry = counts.entry(format!("{source}:{system}:{status}")).or_default();
        entry.0 += 1;
        entry.1 += uses;
        if status != "covered" {
            output.write_record([source, system, code, &uses.to_string(), status, &candidates.iter().map(i64::to_string).collect::<Vec<_>>().join("|")])?;
        }
        Ok(())
    };
    for row in reference.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (system, selector) = row?;
        // Prefix patterns and DDC summary spans are selector metadata, not
        // individual book codes. LCC stored ranges use '..', but printed
        // classification intervals use '-'.
        let selector = selector.split('@').next().unwrap();
        if selector.contains('*') || (system == "ddc" && (selector.len() != 3 || selector == "991" || selector == "992")) {
            continue;
        }
        let notation = if system == "lcc" { selector.replace("..", "-") } else { selector.to_owned() };
        check("master", &system, &notation, 0)?;
    }
    let usage = rusqlite::Connection::open_with_flags(&args[3], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut observed = usage.prepare("SELECT notation,uses FROM code_usage WHERE scheme=2 AND upper(ltrim(notation)) GLOB '[DEF]*' ORDER BY uses DESC,notation")?;
    for row in observed.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (code, uses) = row?;
        check("observed_DEF", "lcc", &code, uses)?;
    }
    output.flush()?;
    for (key, (strings, uses)) in counts {
        eprintln!("{key}\t{strings}\t{uses}");
    }
    Ok(())
}
