//! Shared bibliographic lookup preparation and checksum-valid filename ISBNs.
pub use metadata_contract::identity_evidence::{edition_identity_evidence, EditionIdentityEvidence};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_explicit_filename_author() {
        assert_eq!(edition_identity_evidence("An Illustrated Guide (by-Kenneth-Ray-Stubbs)"), EditionIdentityEvidence { title: "An Illustrated Guide".to_owned(), author: Some("Kenneth Ray Stubbs".to_owned()), book_year: None });
    }

    #[test]
    fn extracts_plain_person_parenthesis_conservatively() {
        assert_eq!(edition_identity_evidence("An Illustrated Guide (Kenneth Ray Stubbs)").author.as_deref(), Some("Kenneth Ray Stubbs"));
        assert_eq!(edition_identity_evidence("Physical Chemistry (Second Edition)").author, None);
    }

    #[test]
    fn extracts_noisy_filename_title_author_and_year() {
        assert_eq!(
            edition_identity_evidence("(it-ebooks-2019) it-ebooks - Algorithms (Jeff Erickson)-iBooker it-ebooks (2019)(1)"),
            EditionIdentityEvidence { title: "Algorithms".to_owned(), author: Some("Jeff Erickson".to_owned()), book_year: Some(2019) }
        );
    }

    #[test]
    fn extracts_inline_by_author_before_source_tag() {
        assert_eq!(edition_identity_evidence("A Concise History of France by Roger Price (z-lib.org)"), EditionIdentityEvidence { title: "A Concise History of France".to_owned(), author: Some("Roger Price".to_owned()), book_year: None });
    }

    #[test]
    fn lookup_cleanup_preserves_qualifiers_and_removes_archive_debris() {
        let evidence = edition_identity_evidence("(by-Anne-Hooper)-Great-Sex-Guide-692141-(z-lib.org)");
        assert_eq!(evidence.title, "Great-Sex-Guide");
        assert_eq!(evidence.author.as_deref(), Some("Anne Hooper"));
        let evidence = edition_identity_evidence("A Concise History of Switzerland (Cambridge Concise Histories)");
        assert_eq!(evidence.title, "A Concise History of Switzerland");
        assert!(evidence.author.is_none());
        assert_eq!(edition_identity_evidence("History (Volume 2)").title, "History (Volume 2)");
        assert_eq!(edition_identity_evidence("History (Revised Edition)").title, "History (Revised Edition)");
    }
}
