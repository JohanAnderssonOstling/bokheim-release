//! Read taxonomy definitions identically during artifact compilation and overlay loading.
use super::runtime::UnifiedConceptDefinition;
use rusqlite::Connection;
use std::collections::BTreeMap;

pub fn read_unified_taxonomy_connection(connection: &Connection, source: &str) -> Result<Vec<UnifiedConceptDefinition>, String> {
    connection.pragma_update(None, "foreign_keys", true).map_err(|error| format!("failed to enable taxonomy database constraints {source}: {error}"))?;
    let integrity = connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)).map_err(|error| format!("failed to verify taxonomy database {source}: {error}"))?;
    if integrity != "ok" {
        return Err(format!("unified taxonomy database failed its integrity check {source}: {integrity}"));
    }
    let foreign_key_violations = connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get::<_, i64>(0)).map_err(|error| format!("failed to verify taxonomy database relationships {source}: {error}"))?;
    if foreign_key_violations != 0 {
        return Err(format!("unified taxonomy database contains {foreign_key_violations} invalid relationships: {source}"));
    }
    let format_version = connection.query_row("SELECT value FROM taxonomy_meta WHERE key='format_version'", [], |row| row.get::<_, String>(0)).map_err(|error| format!("failed to read taxonomy database format {source}: {error}"))?;
    if format_version != "1" {
        return Err(format!("unsupported unified taxonomy database format {format_version} in {source}"));
    }

    let mut parents = BTreeMap::<i64, Vec<i64>>::new();
    {
        let mut statement = connection.prepare("SELECT concept_id,parent_concept_id FROM concept_parent ORDER BY concept_id,ordinal").map_err(|error| format!("failed to prepare taxonomy parents {source}: {error}"))?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).map_err(|error| format!("failed to query taxonomy parents {source}: {error}"))?;
        for row in rows {
            let (concept_id, parent_id) = row.map_err(|error| format!("failed to read taxonomy parent {source}: {error}"))?;
            parents.entry(concept_id).or_default().push(parent_id);
        }
    }
    let mut selectors = BTreeMap::<i64, BTreeMap<String, Vec<String>>>::new();
    for (system_id, table) in [("bisac", "bisac_selector"), ("ddc", "ddc_selector"), ("lcc", "lcc_selector")] {
        let mut statement = connection.prepare(&format!("SELECT concept_id,selector FROM {table} ORDER BY concept_id,selector")).map_err(|error| format!("failed to prepare taxonomy {system_id} selectors {source}: {error}"))?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(|error| format!("failed to query taxonomy {system_id} selectors {source}: {error}"))?;
        for row in rows {
            let (concept_id, selector) = row.map_err(|error| format!("failed to read taxonomy {system_id} selector {source}: {error}"))?;
            selectors.entry(concept_id).or_default().entry(system_id.to_owned()).or_default().push(selector);
        }
    }
    {
        for (concept_id, start, end) in super::range_storage::read_ranges(connection)? {
            selectors.entry(concept_id).or_default().entry("lcc".to_owned()).or_default().push(format!("{start}..{end}"));
        }
    }

    let mut statement = connection.prepare("SELECT concept_id,preferred_label FROM concept ORDER BY rowid").map_err(|error| format!("failed to prepare taxonomy concepts {source}: {error}"))?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(|error| format!("failed to query taxonomy concepts {source}: {error}"))?;
    let mut concepts = Vec::new();
    for row in rows {
        let (concept_id, preferred_label) = row.map_err(|error| format!("failed to read taxonomy concept {source}: {error}"))?;
        let parent_ids = parents.remove(&concept_id).unwrap_or_default();
        concepts.push(UnifiedConceptDefinition::new(concept_id.clone(), preferred_label, parent_ids, selectors.remove(&concept_id).unwrap_or_default()));
    }
    Ok(concepts)
}
