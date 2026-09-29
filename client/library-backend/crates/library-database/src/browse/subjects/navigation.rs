//! Resolve a single route with indexed parent/child lookups. Bulk readers use SQL recursion.
use crate::{Database, DatabaseError};
use rusqlite::OptionalExtension;
use std::collections::HashSet;
use subject_projection::ROOT_SUBJECT_PATH;

impl Database {
    pub(crate) fn subject_route_id(&self, path: &str) -> Result<Option<i64>, DatabaseError> {
        if path == ROOT_SUBJECT_PATH || path.is_empty() {
            return Ok(Some(0));
        }
        let mut lookup = self.connection.prepare_cached(include_str!("../sql/subjects/child.sql"))?;
        // A label can itself contain the display separator (e.g. Myanmar / Burma).
        // Follow matching whole labels, retaining alternatives when prefixes overlap.
        let mut pending = vec![(0, path)];
        while let Some((parent, remaining)) = pending.pop() {
            let mut children = lookup.query(rusqlite::params![parent, remaining])?;
            while let Some(child) = children.next()? {
                let route: i64 = child.get(0)?;
                let label: String = child.get(1)?;
                if remaining == label {
                    return Ok(Some(route));
                }
                if let Some(rest) = remaining.strip_prefix(&label).and_then(|suffix| suffix.strip_prefix(" / ")) {
                    pending.push((route, rest));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn subject_route_path(&self, route: i64) -> Result<Option<String>, DatabaseError> {
        if route == 0 {
            return Ok(Some(ROOT_SUBJECT_PATH.to_owned()));
        }
        // Curated is immutable for the connection, so this walk needs no read transaction.
        let mut lookup = self.connection.prepare_cached(include_str!("../sql/subjects/parent.sql"))?;
        let mut current = route;
        let mut seen = HashSet::new();
        let mut labels = Vec::new();
        while current != 0 {
            if !seen.insert(current) {
                return Err(DatabaseError::message("cycle in subject route ancestry"));
            }
            let node = lookup.query_row([current], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).optional()?;
            let Some((parent, label)) = node else { return Ok(None) };
            labels.push(label);
            current = parent;
        }
        labels.reverse();
        Ok(Some(labels.join(" / ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn route_ids_preserve_every_matcher_path_and_multiple_parent_occurrences() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let expected =
            subject_projection::unified_taxonomy_routes().into_iter().filter(|r| r.path() != ROOT_SUBJECT_PATH).map(|r| (r.path().to_owned(), (r.concept_id(), r.parent_path().to_owned(), r.label().to_owned()))).collect::<BTreeMap<_, _>>();
        let mut rows = db.connection.prepare("SELECT route_id,path,concept_id,parent_path,label FROM curated.unified_concept_paths").unwrap();
        let routes = rows.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, (r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let actual = routes.iter().map(|(_, path, data)| (path.clone(), data.clone())).collect::<BTreeMap<_, _>>();
        assert_eq!(actual, expected, "route IDs must preserve the matcher/browse contract");
        for (id, path, _) in &routes {
            assert_eq!(db.subject_route_path(*id).unwrap().as_deref(), Some(path.as_str()));
            assert_eq!(db.subject_route_id(path).unwrap(), Some(*id), "{path}");
        }
        let distinct_concepts = routes.iter().map(|(_, _, (concept, _, _))| *concept).collect::<HashSet<_>>();
        assert!(routes.len() > distinct_concepts.len(), "fixture exercises concepts with multiple routes");
        assert_eq!(db.subject_route_path(0).unwrap().as_deref(), Some(ROOT_SUBJECT_PATH));
        assert_eq!(db.subject_route_path(-1).unwrap(), None);
        assert_eq!(db.subject_route_id("No such subject / child").unwrap(), None);
        let stored_text_columns: i64 = db.connection.query_row("SELECT count(*) FROM pragma_table_info('unified_concept_route','curated') WHERE type='TEXT'", [], |r| r.get(0)).unwrap();
        assert_eq!(stored_text_columns, 0);
    }
}
