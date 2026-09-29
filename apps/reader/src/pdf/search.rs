//! PDF search query policy.

/// Searching starts at two Unicode characters. Shorter input still produces
/// an empty query so old results and in-flight work are cleared.
pub(super) fn renderer_query(input: &str) -> String {
    if input.chars().count() >= 2 { input.to_owned() } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::renderer_query;

    #[test]
    fn shortening_below_the_threshold_clears_renderer_results() {
        let renderer_queries = ["ab", "a"].map(renderer_query);
        assert_eq!(renderer_queries, ["ab".to_owned(), String::new()]);
    }

    #[test]
    fn threshold_counts_unicode_characters() {
        assert_eq!(renderer_query("å"), "");
        assert_eq!(renderer_query("åb"), "åb");
    }
}
