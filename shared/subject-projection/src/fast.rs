use super::runtime::{normalize_source_label, UnifiedTaxonomyRuntime};
use book_model::BookSubject;
use std::collections::BTreeSet;

pub(super) fn matching_hierarchy_paths(runtime: &UnifiedTaxonomyRuntime, subject: &BookSubject) -> BTreeSet<String> {
    let source = subject.source().trim().to_ascii_lowercase();
    let authority = subject.authority().map(str::trim).unwrap_or_default().to_ascii_lowercase();
    if authority != "fast" && !source.ends_with("/#fast") {
        return BTreeSet::new();
    }
    let mut components = subject.name().split("--").map(normalize_source_label).filter(|component| !component.is_empty()).collect::<Vec<_>>();
    if components.len() < 2 {
        return BTreeSet::new();
    }
    components.sort();
    // A matching route must end in one of the heading's labels. Use the label
    // index to avoid constructing and searching every path in the taxonomy.
    let candidates = components.iter().filter_map(|label| runtime.labels.get(label)).flatten().flat_map(|concept| runtime.routes.ids_for_concept(*concept)).copied().collect::<BTreeSet<_>>();
    candidates.into_iter().filter(|id| components_fit_route_suffix(runtime, &components, *id)).map(|id| runtime.routes.path(id)).collect()
}

fn components_fit_route_suffix(runtime: &UnifiedTaxonomyRuntime, components: &[String], id: usize) -> bool {
    let mut labels = Vec::with_capacity(components.len());
    let mut current = Some(id);
    for _ in components {
        let Some(route) = current else { return false };
        labels.push(normalize_source_label(runtime.routes.label(route)));
        current = runtime.routes.parent(route);
    }
    labels.sort();
    labels == components
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_index::{build_runtime, UnifiedConceptDefinition};
    use std::collections::BTreeMap;

    fn runtime() -> UnifiedTaxonomyRuntime {
        let concepts = [(1, "History", vec![]), (2, "Geography", vec![]), (3, "Myanmar / Burma", vec![1, 2]), (4, "Modern", vec![3])]
            .into_iter()
            .map(|(id, label, parents)| UnifiedConceptDefinition::new(id, label.into(), parents, BTreeMap::new()))
            .collect();
        build_runtime(concepts, false).unwrap()
    }

    fn heading(name: &str) -> BookSubject {
        BookSubject::new(None, name, "dc:subject", Some("fast".into()), None).unwrap()
    }

    #[test]
    fn hierarchy_matching_follows_whole_labels_and_keeps_parallel_routes() {
        let runtime = runtime();
        assert_eq!(matching_hierarchy_paths(&runtime, &heading("Myanmar / Burma -- History")), BTreeSet::from(["History / Myanmar / Burma".into()]));
        assert_eq!(matching_hierarchy_paths(&runtime, &heading("Modern -- Myanmar / Burma")), BTreeSet::from(["Geography / Myanmar / Burma / Modern".into(), "History / Myanmar / Burma / Modern".into(),]));
        assert!(matching_hierarchy_paths(&runtime, &heading("History -- Modern")).is_empty(), "components must cover adjacent route nodes");
    }

    #[test]
    fn repeated_components_require_separate_ancestor_occurrences() {
        let runtime = runtime();
        assert!(matching_hierarchy_paths(&runtime, &heading("History -- History")).is_empty());
        let repeated = build_runtime(
            vec![
                UnifiedConceptDefinition::new(1, "History".into(), vec![], BTreeMap::new()),
                UnifiedConceptDefinition::new(2, "Places".into(), vec![1], BTreeMap::new()),
                UnifiedConceptDefinition::new(3, "History".into(), vec![2], BTreeMap::new()),
            ],
            false,
        )
        .unwrap();
        assert_eq!(matching_hierarchy_paths(&repeated, &heading("History -- Places -- History")), BTreeSet::from(["History / Places / History".into()]));
        assert!(matching_hierarchy_paths(&repeated, &heading("History -- History -- History")).is_empty());
    }
}
