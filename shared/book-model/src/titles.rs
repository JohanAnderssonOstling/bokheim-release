//! Title normalization shared by inspection and library ingestion.
use crate::BookRecord;

impl BookRecord {
    pub fn subtitle(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }

    pub fn set_subtitle(&mut self, subtitle: &str) {
        let subtitle = subtitle.trim();
        self.subtitle = (!subtitle.is_empty()).then(|| subtitle.to_owned());
    }

    /// A known subtitle wins: strip a title suffix that already restates it.
    /// Otherwise split once on the first colon, retaining further colons in
    /// the subtitle.
    pub fn normalize_title(&mut self) {
        let original = self.title.trim().to_owned();
        if let Some(subtitle) = self.subtitle.as_deref() {
            if let Some(prefix) = original.strip_suffix(subtitle).and_then(|prefix| prefix.trim_end().strip_suffix(':')) {
                if !prefix.trim().is_empty() {
                    self.title = prefix.trim().to_owned();
                }
            }
            return;
        }
        let Some((main, tail)) = original.split_once(':') else { return };
        let (main, tail) = (main.trim(), tail.trim());
        if main.is_empty() || tail.is_empty() {
            return;
        }
        self.subtitle = Some(tail.to_owned());
        self.title = main.to_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn book(title: &str) -> BookRecord {
        BookRecord { title: title.into(), subtitle: None, contributors: vec![], description: String::new(), book: Default::default() }
    }
    #[test]
    fn colon_split_preserves_evidence_and_is_idempotent() {
        let mut book = book("  Why Machines Learn : The Math: An Introduction  ");
        book.normalize_title();
        assert_eq!(book.title, "Why Machines Learn");
        assert_eq!(book.subtitle(), Some("The Math: An Introduction"));
        let before = book.clone();
        book.normalize_title();
        assert_eq!(book, before);
    }
    #[test]
    fn explicit_subtitle_wins_and_main_title_colons_are_preserved() {
        let mut book = book("Star Trek: Voyager");
        book.set_subtitle("A Guide");
        book.normalize_title();
        assert_eq!(book.title, "Star Trek: Voyager");
        assert_eq!(book.subtitle(), Some("A Guide"));
        book.title = "Star Trek: Voyager: A Guide".into();
        book.normalize_title();
        assert_eq!(book.title, "Star Trek: Voyager");
    }
    #[test]
    fn empty_parts_and_titles_without_colons_remain_unchanged() {
        for title in ["Frankenstein", ": subtitle", "Title: ", ""] {
            let mut book = book(title);
            book.normalize_title();
            assert_eq!(book.title, title);
            assert_eq!(book.subtitle(), None);
        }
    }
}
