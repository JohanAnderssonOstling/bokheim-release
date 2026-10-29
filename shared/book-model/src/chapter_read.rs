use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{audiobook_toc_position, pdf_toc_page, BookTocEntry, ReadingPosition};

/// Leaves finish only after being entered and passed. A parent finishes when
/// all its children have finished; jumping over unopened leaves leaves them unread.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterReadStatus {
    pub completed: BTreeSet<String>,
    #[serde(default)]
    active: Option<String>,
}

impl ChapterReadStatus {
    pub fn active_target(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn reset(&mut self) {
        self.completed.clear();
    }

    /// Every chapter read, or none, and none under way: what marking the whole
    /// book read or unread leaves.
    pub fn mark_all(&mut self, entries: &[BookTocEntry], read: bool) {
        fn complete(entries: &[BookTocEntry], completed: &mut BTreeSet<String>) {
            for entry in entries {
                if !entry.target.is_empty() {
                    completed.insert(entry.target.clone());
                }
                complete(&entry.children, completed);
            }
        }
        self.completed.clear();
        if read {
            complete(entries, &mut self.completed);
        }
        self.active = None;
    }

    pub fn reset_chapter(&mut self, entries: &[BookTocEntry], target: &str) {
        fn clear(entries: &[BookTocEntry], target: &str, inherited: bool, completed: &mut BTreeSet<String>) {
            for entry in entries {
                let reset = inherited || entry.target == target;
                if reset {
                    completed.remove(&entry.target);
                }
                clear(&entry.children, target, reset, completed);
            }
        }
        clear(entries, target, false, &mut self.completed);
        self.update_parents(entries);
    }

    pub fn update_parents(&mut self, entries: &[BookTocEntry]) -> bool {
        let mut changed = false;
        for entry in entries {
            if entry.children.is_empty() {
                continue;
            }
            changed |= self.update_parents(&entry.children);
            if entry.children.iter().all(|child| self.completed.contains(&child.target)) {
                changed |= self.completed.insert(entry.target.clone());
            } else {
                changed |= self.completed.remove(&entry.target);
            }
        }
        changed
    }

    pub fn update_position(&mut self, entries: &[BookTocEntry], position: &ReadingPosition, entry: Option<&str>, at_end: bool) -> bool {
        fn target_at<'a>(entries: &'a [BookTocEntry], position: &ReadingPosition, best: &mut Option<(u64, &'a str)>) {
            for entry in entries {
                let start = match position {
                    ReadingPosition::Audiobook(_) => audiobook_toc_position(&entry.target),
                    ReadingPosition::Pdf(_) | ReadingPosition::PdfAtPoint { .. } => pdf_toc_page(&entry.target).map(|page| page as u64),
                    ReadingPosition::Cbz { .. } => crate::cbz_toc_page(&entry.target).map(|page| page as u64),
                    _ => None,
                };
                let current = position.audiobook_position_millis().or_else(|| position.pdf_page_index().map(u64::from)).or_else(|| match position { ReadingPosition::Cbz { page, .. } => Some(u64::from(*page)), _ => None });
                if let Some(start) = start.filter(|start| current.is_some_and(|current| *start <= current)) {
                    if best.is_none_or(|(previous, _)| start >= previous) {
                        *best = Some((start, &entry.target));
                    }
                }
                target_at(&entry.children, position, best);
            }
        }
        let mut target = None;
        target_at(entries, position, &mut target);
        self.update(entries, target.map(|(_, target)| target).or(entry), at_end)
    }

    pub fn update(&mut self, entries: &[BookTocEntry], target: Option<&str>, at_end: bool) -> bool {
        fn flatten<'a>(entries: &'a [BookTocEntry], ordered: &mut Vec<(&'a str, usize)>) {
            for entry in entries {
                let index = ordered.len();
                ordered.push((&entry.target, index + 1));
                flatten(&entry.children, ordered);
                ordered[index].1 = ordered.len();
            }
        }
        let mut ordered = Vec::new();
        flatten(entries, &mut ordered);
        let Some(current) = target.and_then(|target| ordered.iter().position(|(id, _)| *id == target)) else { return false };
        let previous = self.active.as_deref().and_then(|active| ordered.iter().position(|(id, _)| *id == active));
        let mut changed = false;
        if let Some(previous) = previous {
            for (index, (id, end)) in ordered.iter().enumerate() {
                // Complete only the leaf we were inside. Parents derive their
                // status from their children; skipped leaves remain unread.
                let entered = index == previous && *end == index + 1;
                let passed_end = *end <= current || (at_end && *end == ordered.len());
                if entered && passed_end && !id.is_empty() {
                    changed |= self.completed.insert((*id).to_owned());
                }
            }
        }
        self.active = target.filter(|_| !at_end).map(str::to_owned);
        changed |= self.update_parents(entries);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapters() -> Vec<BookTocEntry> {
        (0..5).map(|index| BookTocEntry { title: format!("Chapter {index}"), target: crate::audiobook_toc_target(index * 1000), children: vec![] }).collect()
    }

    #[test]
    fn opening_in_the_middle_does_not_complete_earlier_chapters() {
        let mut status = ChapterReadStatus::default();
        let toc = chapters();
        status.update(&toc, Some(&toc[2].target), false);
        assert!(status.completed.is_empty());
        status.update(&toc, Some(&toc[4].target), false);
        assert_eq!(status.completed, BTreeSet::from([toc[2].target.clone()]));
        status.update(&toc, Some(&toc[0].target), false);
        assert_eq!(status.completed.len(), 1);
    }

    #[test]
    fn reset_keeps_position_and_only_new_end_crossings_complete_chapters() {
        let mut status = ChapterReadStatus::default();
        let toc = chapters();
        status.update(&toc, Some(&toc[1].target), false);
        status.update(&toc, Some(&toc[2].target), false);
        status.reset();
        status.update(&toc, Some(&toc[2].target), false);
        assert!(status.completed.is_empty());
        status.update(&toc, Some(&toc[3].target), false);
        assert_eq!(status.completed, BTreeSet::from([toc[2].target.clone()]));
        status.update(&toc, Some(&toc[4].target), false);
        status.update(&toc, Some(&toc[4].target), true);
        assert!(status.completed.contains(&toc[4].target));
    }

    #[test]
    fn parent_finishes_at_its_end_without_finishing_unopened_children() {
        let mut toc = chapters();
        toc[0].children = vec![BookTocEntry { title: "Section".into(), target: "section".into(), children: vec![] }];
        let mut status = ChapterReadStatus::default();
        status.update(&toc, Some("section"), false);
        status.update(&toc, Some(&toc[2].target), false);
        assert_eq!(status.completed, BTreeSet::from([toc[0].target.clone(), "section".into()]));
    }

    #[test]
    fn audio_boundaries_finish_only_the_chapter_that_was_entered() {
        let toc = chapters();
        let mut status = ChapterReadStatus::default();
        status.update_position(&toc, &ReadingPosition::audiobook_millis(2500), None, false);
        status.update_position(&toc, &ReadingPosition::audiobook_millis(2999), None, false);
        assert!(status.completed.is_empty());
        status.update_position(&toc, &ReadingPosition::audiobook_millis(4000), None, false);
        assert_eq!(status.completed, BTreeSet::from([toc[2].target.clone()]));
        let restored: ChapterReadStatus = serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
        assert_eq!(restored, status);
    }

    #[test]
    fn skipping_from_chapter_five_to_ten_leaves_six_through_nine_unread() {
        for format in ["epub", "pdf", "audiobook"] {
            let toc: Vec<_> = (0..12)
                .map(|index| BookTocEntry {
                    title: format!("Chapter {}", index + 1),
                    target: match format {
                        "pdf" => crate::pdf_toc_target(index * 10),
                        "audiobook" => crate::audiobook_toc_target(index as u64 * 1000),
                        _ => format!("chapter-{}.xhtml", index + 1),
                    },
                    children: vec![],
                })
                .collect();
            let mut status = ChapterReadStatus::default();
            for index in [4, 9] {
                let position = match format {
                    "pdf" => ReadingPosition::pdf_page(index as u32 * 10 + 5),
                    "audiobook" => ReadingPosition::audiobook_millis(index as u64 * 1000 + 500),
                    _ => ReadingPosition::epub_cfi(&format!("epubcfi(/6/{})", (index + 1) * 2)).unwrap(),
                };
                status.update_position(&toc, &position, Some(&toc[index].target), false);
            }
            assert_eq!(status.completed, BTreeSet::from([toc[4].target.clone()]), "{format}: only the chapter navigated out of should finish");
            assert_eq!(status.active_target(), Some(toc[9].target.as_str()));
            assert!(toc[5..9].iter().all(|chapter| !status.completed.contains(&chapter.target)), "{format}: skipped chapters must remain unread");
        }
    }

    #[test]
    fn jumping_straight_to_the_end_does_not_finish_the_unopened_final_chapter() {
        let toc = chapters();
        let mut status = ChapterReadStatus::default();
        status.update(&toc, Some(&toc[1].target), false);
        status.update(&toc, Some(&toc[4].target), true);
        status.update(&toc, Some(&toc[4].target), true);
        assert_eq!(status.completed, BTreeSet::from([toc[1].target.clone()]));
        status.reset();
        status.update(&toc, Some(&toc[4].target), true);
        assert!(status.completed.is_empty());
    }

    #[test]
    fn parents_require_all_children_and_resetting_one_child_keeps_its_siblings_read() {
        let leaf = |target: &str| BookTocEntry { title: target.into(), target: target.into(), children: vec![] };
        let toc = vec![BookTocEntry { title: "Book".into(), target: "book".into(), children: vec![BookTocEntry { title: "Part".into(), target: "part".into(), children: vec![leaf("one"), leaf("two")] }, leaf("three")] }, leaf("after")];
        let mut status = ChapterReadStatus::default();
        status.update(&toc, Some("part"), false);
        status.update(&toc, Some("after"), false);
        assert!(status.completed.is_empty());
        status.update(&toc, Some("one"), false);
        status.update(&toc, Some("after"), false);
        assert_eq!(status.completed, BTreeSet::from(["one".into()]));
        status.update(&toc, Some("two"), false);
        status.update(&toc, Some("three"), false);
        assert!(status.completed.contains("part"));
        assert!(!status.completed.contains("book"));
        status.update(&toc, Some("after"), false);
        assert!(status.completed.contains("book"));
        status.reset_chapter(&toc, "one");
        assert_eq!(status.completed, BTreeSet::from(["two".into(), "three".into()]));
        assert_eq!(status.active_target(), Some("after"));
        status.update(&toc, Some("after"), false);
        assert!(!status.completed.contains("one"));
        status.reset_chapter(&toc, "part");
        assert_eq!(status.completed, BTreeSet::from(["three".into()]));
    }
}
