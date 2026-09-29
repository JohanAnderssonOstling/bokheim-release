//! Structured range storage. Full boundary strings exist only at the API/editor boundary.
use super::lcc::lcc_endpoint;
use super::runtime::cutter_components;
use rusqlite::{params, Connection};

struct Boundary {
    letters: String,
    // NULL means the entire letter class: zero at the lower end, infinity at the upper end.
    number: Option<f64>,
    cutters: Vec<(String, String)>,
}

impl Boundary {
    fn parse(text: &str, upper: bool) -> Result<Self, String> {
        let endpoint = lcc_endpoint(text, upper).ok_or_else(|| format!("invalid stored LCC boundary {text}"))?;
        Ok(Self { number: (endpoint.code != endpoint.letters).then_some(endpoint.number), letters: endpoint.letters, cutters: cutter_components(&endpoint.remainder) })
    }

    fn format(&self) -> String {
        let mut text = self.letters.clone();
        if let Some(number) = self.number {
            text.push_str(&number.to_string());
        }
        for (letters, digits) in &self.cutters {
            text.push('.');
            text.push_str(letters);
            text.push_str(digits);
        }
        text
    }
}

pub(super) fn insert_range(connection: &Connection, concept_id: i64, start: &str, end: &str) -> Result<(), String> {
    let start = Boundary::parse(start, false)?;
    let end = Boundary::parse(end, true)?;
    connection
        .prepare_cached("INSERT INTO lcc_range(concept_id,start_letters,start_number,start_cutters,end_letters,end_number,end_cutters) VALUES(?1,?2,?3,?4,?5,?6,?7)")
        .map_err(|e| e.to_string())?
        .execute(params![concept_id, start.letters, start.number, serde_json::to_string(&start.cutters).map_err(|e| e.to_string())?, end.letters, end.number, serde_json::to_string(&end.cutters).map_err(|e| e.to_string())?])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn read_ranges(connection: &Connection) -> Result<Vec<(i64, String, String)>, String> {
    let mut statement = connection
        .prepare("SELECT concept_id,start_letters,start_number,start_cutters,end_letters,end_number,end_cutters FROM lcc_range ORDER BY concept_id,start_letters,start_number,start_cutters,end_letters,end_number,end_cutters")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, Option<f64>>(5)?, r.get::<_, String>(6)?)))
        .map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for row in rows {
        let (id, sl, sn, sc, el, en, ec) = row.map_err(|e| e.to_string())?;
        let start = Boundary { letters: sl, number: sn, cutters: serde_json::from_str(&sc).map_err(|e| e.to_string())? };
        let end = Boundary { letters: el, number: en, cutters: serde_json::from_str(&ec).map_err(|e| e.to_string())? };
        let (start_text, end_text) = (start.format(), end.format());
        // Fail explicitly on malformed persisted structures as well as malformed text.
        for (boundary, text, upper) in [(&start, &start_text, false), (&end, &end_text, true)] {
            let parsed = Boundary::parse(text, upper)?;
            if boundary.letters != parsed.letters || boundary.number != parsed.number || boundary.cutters != parsed.cutters {
                return Err(format!("noncanonical structured LCC boundary for concept {id}"));
            }
        }
        result.push((id, start_text, end_text));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_storage_distinguishes_class_bounds_and_keeps_multiple_cutters() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE concept(concept_id INTEGER PRIMARY KEY); INSERT INTO concept VALUES(1),(2);").unwrap();
        connection.execute_batch(include_str!("range_storage.sql")).unwrap();
        for (start, end) in [("T", "TX"), ("T0", "T0"), ("DA890.E3.A2", "DA890.E3.Z"), ("QA1.2.A01", "QA1.2.A019")] {
            insert_range(&connection, 1, start, end).unwrap();
        }
        let rows = read_ranges(&connection).unwrap();
        assert!(rows.contains(&(1, "T".into(), "TX".into())));
        assert!(rows.contains(&(1, "T0".into(), "T0".into())));
        assert!(rows.contains(&(1, "DA890.E3.A2".into(), "DA890.E3.Z".into())));
        assert!(rows.contains(&(1, "QA1.2.A01".into(), "QA1.2.A019".into())));
        assert!(insert_range(&connection, 2, "T", "TX").is_err());
        assert!(insert_range(&connection, 2, "QA1.2.A010", "QA1.2.A0190").is_err());
    }
}
