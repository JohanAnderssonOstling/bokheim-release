//! What a [`BookTocEntry::target`] means, per format.
//!
//! Every format's navigation reaches the same tree of title, target and
//! children; what differs is only the target value. An EPUB target needs no
//! prefix — it is the publisher's own href, and resolving it to a document and
//! anchor is the renderer's business. A PDF entry points at a page rather than
//! at a document fragment, and an audiobook chapter at an offset, so both carry
//! their value behind a scheme.
//!
//! [`BookTocEntry::target`]: crate::BookTocEntry::target

pub const AUDIOBOOK_TOC_TARGET_PREFIX: &str = "audiobook-position-ms:";

pub fn audiobook_toc_target(position_ms: u64) -> String {
    format!("{AUDIOBOOK_TOC_TARGET_PREFIX}{position_ms}")
}

pub fn audiobook_toc_position(target: &str) -> Option<u64> {
    target.strip_prefix(AUDIOBOOK_TOC_TARGET_PREFIX)?.parse().ok()
}

/// Display cleanup for an audiobook entry, using its one-based position.
/// Keep the original stored heading as matching evidence.
pub fn audiobook_chapter_display_title(title: &str, position: usize) -> String {
    let spaced = title.replace('_', " ");
    let clean = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let digits = clean.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && clean[..digits].parse::<usize>().ok() == Some(position) {
        let remainder = &clean[digits..];
        if remainder.is_empty() {
            return format!("Chapter {position}");
        }
        let separator = |c: char| c.is_whitespace() || matches!(c, '.' | '-' | ':' | ')');
        if remainder.starts_with(separator) {
            let name = remainder.trim_start_matches(separator);
            if !name.is_empty() {
                return name.to_owned();
            }
        }
    }
    clean
}

pub const PDF_TOC_TARGET_PREFIX: &str = "pdf-page:";

pub fn pdf_toc_target(page_index: usize) -> String {
    format!("{PDF_TOC_TARGET_PREFIX}{page_index}")
}

pub fn pdf_toc_page(target: &str) -> Option<usize> {
    target.strip_prefix(PDF_TOC_TARGET_PREFIX)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_display_removes_only_a_matching_separate_ordinal() {
        assert_eq!(audiobook_chapter_display_title("05_Aristotle", 5), "Aristotle");
        assert_eq!(audiobook_chapter_display_title("5.  The_Russian_Empire", 5), "The Russian Empire");
        assert_eq!(audiobook_chapter_display_title("05_Aristotle", 4), "05 Aristotle");
        assert_eq!(audiobook_chapter_display_title("5th_Century", 5), "5th Century");
        assert_eq!(audiobook_chapter_display_title("1917_Revolution", 5), "1917 Revolution");
        assert_eq!(audiobook_chapter_display_title("005", 5), "Chapter 5");
        assert_eq!(audiobook_chapter_display_title("Chapter_5", 5), "Chapter 5");
        assert_eq!(audiobook_chapter_display_title("Paul_Bushkovitch_-_A_Concise_History", 5), "Paul Bushkovitch - A Concise History");
    }

    #[test]
    fn toc_targets_round_trip_and_reject_other_formats() {
        let audiobook_target = audiobook_toc_target(3_726_500);
        assert_eq!(audiobook_target, "audiobook-position-ms:3726500");
        assert_eq!(audiobook_toc_position(&audiobook_target), Some(3_726_500));
        assert_eq!(audiobook_toc_position("chapter.xhtml"), None);
        assert_eq!(audiobook_toc_position("audiobook-position-ms:not-a-number"), None);

        let pdf_target = pdf_toc_target(42);
        assert_eq!(pdf_target, "pdf-page:42");
        assert_eq!(pdf_toc_page(&pdf_target), Some(42));
        assert_eq!(pdf_toc_page("chapter.xhtml#part2"), None);
        assert_eq!(pdf_toc_page(&audiobook_target), None);
        assert_eq!(pdf_toc_page("pdf-page:"), None);
    }
}
