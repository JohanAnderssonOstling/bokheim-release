//! The table of contents, and which of its entries the reader is inside.
//!
//! TOC links are hrefs written by the publisher; the renderer resolves each to
//! a source position. Highlighting the current chapter means matching the
//! settled page's source address back to a link. Resolutions stay in TOC
//! order so duplicate targets and document-level fallbacks are deterministic.

use std::{collections::HashMap, sync::Arc};

use book_model::{BookTocEntry, ChapterReadStatus};
use html::document::{MarkupSyntax, decode_html_bytes, parse_document};
use html::resources::ResourceProvider;
use skrift_column_paginator::cfi::parse_source_position;
use skrift_column_paginator::{SourcePosition, TocEntry};

use crate::shell::annotation_tree::{AnnotationBranch, AnnotationTree};

/// Where a TOC link points, as `(document index, optional anchor)`.
pub(crate) type Resolution = (usize, Option<String>);
pub(crate) type SourcePositions = HashMap<usize, Vec<(Vec<usize>, usize)>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTocEntry {
    link: String,
    document: usize,
    anchor: Option<String>,
}

#[derive(Default)]
pub(crate) struct TocState {
    entries: Arc<Vec<TocEntry>>,
    read_entries: Vec<BookTocEntry>,
    pub(crate) read_status: ChapterReadStatus,
    resolutions: Vec<ResolvedTocEntry>,
    /// The link the reader is currently inside, if any.
    active_link: Option<String>,
    position: Option<SourcePosition>,
    source_positions: SourcePositions,
    document_count: usize,
}

impl TocState {
    pub(crate) fn set(&mut self, entries: Vec<TocEntry>, resolutions: Vec<ResolvedTocEntry>) {
        fn convert(entries: &[TocEntry]) -> Vec<BookTocEntry> {
            entries.iter().map(|entry| BookTocEntry { title: entry.title.clone(), target: entry.link.clone(), children: convert(&entry.children) }).collect()
        }
        self.read_entries = convert(&entries);
        self.entries = Arc::new(entries);
        self.resolutions = resolutions;
        self.source_positions.clear();
        self.document_count = self.resolutions.iter().map(|entry| entry.document + 1).max().unwrap_or(0);
        self.refresh_position();
    }

    /// Retain early renderer positions even when the TOC is still loading.
    pub(crate) fn update_position(&mut self, position: &SourcePosition) -> bool {
        self.position = Some(position.clone());
        self.refresh_position()
    }

    pub(crate) fn update_read_status(&mut self, at_end: bool) {
        self.read_status.update(&self.read_entries, self.active_link.as_deref(), at_end);
    }

    pub(crate) fn reset_chapter_read_status(&mut self, target: &str) {
        self.read_status.reset_chapter(&self.read_entries, target);
    }

    fn refresh_position(&mut self) -> bool {
        let link = self.position.as_ref().and_then(|position| self.ordinal_for_position(position)).map(|ordinal| self.resolutions[ordinal].link.clone());
        self.set_active_link(link)
    }

    /// Publisher links are relative to the container root, not the chapter
    /// that happens to be on screen while the metadata request completes.
    pub(crate) fn resolve_publication_href(href: &str, documents: &[String], provider: &dyn ResourceProvider) -> Option<Resolution> {
        let (path, fragment) = href.split_once('#').unwrap_or((href, ""));
        let uri = provider.resolve("", path);
        let mut matches = documents.iter().enumerate().filter(|(_, document)| **document == uri).map(|(index, _)| index);
        let index = matches.next()?;
        matches.next().is_none().then(|| (index, (!fragment.is_empty()).then(|| fragment.to_owned())))
    }

    /// Index TOC anchors in source order for reading position and annotations.
    pub(crate) fn source_positions(resolutions: &[ResolvedTocEntry], documents: &[String], provider: &dyn ResourceProvider) -> SourcePositions {
        let mut positions = HashMap::new();
        for (index, uri) in documents.iter().enumerate() {
            if !resolutions.iter().any(|entry| entry.document == index && entry.anchor.is_some()) {
                continue;
            }
            let Ok(bytes) = provider.read_bytes(uri) else { continue };
            let metadata = provider.metadata(uri).ok();
            let source = decode_html_bytes(&bytes, metadata.as_ref().and_then(|metadata| metadata.charset.as_deref()));
            let syntax = if metadata.as_ref().and_then(|metadata| metadata.media_type.as_deref()) == Some("application/xhtml+xml") { MarkupSyntax::Xml } else { MarkupSyntax::from_uri(uri) };
            let Some(paths) = anchor_source_positions(resolutions, index, &source, syntax) else { continue };
            positions.insert(index, paths);
        }
        positions
    }

    pub(crate) fn install_source_positions(&mut self, positions: SourcePositions, document_count: usize) {
        self.source_positions = positions;
        self.document_count = document_count;
        self.refresh_position();
    }

    pub(crate) fn ordinal_for_cfi(&self, cfi: &str) -> Option<usize> {
        self.ordinal_for_position(&parse_source_position(cfi)?)
    }

    /// Resolve a document without inventing a CFI location inside it.
    pub(crate) fn ordinal_for_document(&self, document: usize) -> Option<usize> {
        self.ordinal_for_position(&SourcePosition { document, path: Vec::new(), utf16_offset: None })
    }

    pub(crate) fn ordinal_for_position(&self, position: &SourcePosition) -> Option<usize> {
        let document = position.document;
        if document >= self.document_count {
            return None;
        }
        let section = self.source_positions.get(&document).and_then(|positions| positions.iter().filter(|(anchor, _)| anchor <= &position.path).max_by(|(left, _), (right, _)| left.cmp(right)).map(|(_, ordinal)| *ordinal));
        section
            .or_else(|| self.resolutions.iter().position(|entry| entry.document == document && entry.anchor.is_none()))
            .or_else(|| self.resolutions.iter().position(|entry| entry.document == document))
            .or_else(|| self.resolutions.iter().enumerate().filter(|(_, entry)| entry.document < document).max_by_key(|(ordinal, entry)| (entry.document, *ordinal)).map(|(ordinal, _)| ordinal))
    }

    /// Navigable roots are chapters; unresolved grouping nodes delegate to
    /// their children. Nested sections remain within their owning chapter.
    pub(in crate::epub) fn chapter_ordinals(&self) -> Vec<usize> {
        fn visit(entries: &[TocEntry], resolved: &[ResolvedTocEntry], result: &mut Vec<usize>) {
            for entry in entries {
                if let Some(index) = resolved.iter().position(|item| item.link == entry.link) {
                    if !result.contains(&index) {
                        result.push(index);
                    }
                } else {
                    visit(&entry.children, resolved, result);
                }
            }
        }
        let mut result = Vec::new();
        visit(&self.entries, &self.resolutions, &mut result);
        result
    }

    #[cfg(any(test, all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo"))))]
    pub(crate) fn chapter_for_cfi(&self, cfi: &str) -> Option<usize> {
        let ordinal = self.ordinal_for_cfi(cfi)?;
        self.chapter_ordinals().into_iter().rev().find(|chapter| *chapter <= ordinal)
    }

    #[cfg(any(test, all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo"))))]
    pub(crate) fn chapter_for_document(&self, document: usize) -> Option<usize> {
        let ordinal = self.ordinal_for_document(document)?;
        self.chapter_ordinals().into_iter().rev().find(|chapter| *chapter <= ordinal)
    }

    #[cfg(any(test, all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo"))))]
    pub(crate) fn narration_excludes_cfi(&self, cfi: &str) -> bool {
        let Some(position) = parse_source_position(cfi) else { return false };
        let Some(ordinal) = self.ordinal_for_position(&position) else { return false };
        let entry = &self.resolutions[ordinal];
        // The sidebar falls back to an earlier entry across documents and
        // before the first anchor. Those fallbacks must not suppress prose.
        if entry.document != position.document {
            return false;
        }
        if entry.anchor.is_some() && !self.source_positions.get(&position.document).is_some_and(|anchors| anchors.iter().any(|(path, index)| *index == ordinal && path <= &position.path)) {
            return false;
        }
        self.title_for_ordinal(ordinal).is_some_and(|title| html::layout::is_navigation_section_title(&title))
    }

    /// The publisher's own href for the entry the reader is inside — the
    /// identity a stored position is recorded against, since a title can be
    /// re-worded by a re-parsed navigation document.
    pub(crate) fn active_link(&self) -> Option<String> {
        self.active_link.clone()
    }

    pub(crate) fn set_active_link(&mut self, active_link: Option<String>) -> bool {
        if self.active_link == active_link {
            return false;
        }
        self.active_link = active_link;
        true
    }

    /// Resolves every link depth-first, preserving the publisher's TOC order
    /// and skipping links the renderer cannot place.
    pub(crate) fn resolve(entries: &[TocEntry], resolve: impl Fn(&str) -> Option<Resolution>) -> Vec<ResolvedTocEntry> {
        fn visit(entries: &[TocEntry], resolve: &impl Fn(&str) -> Option<Resolution>, resolutions: &mut Vec<ResolvedTocEntry>) {
            for entry in entries {
                if let Some((document, anchor)) = resolve(&entry.link) {
                    resolutions.push(ResolvedTocEntry { link: entry.link.clone(), document, anchor });
                }
                visit(&entry.children, resolve, resolutions);
            }
        }

        let mut resolutions = Vec::new();
        visit(entries, &resolve, &mut resolutions);
        resolutions
    }

    /// Groups publisher TOC anchors for the renderer's nearest-preceding-anchor
    /// filter. Anchor order is retained and duplicate targets are omitted.
    pub(crate) fn anchor_strings_by_doc(resolutions: &[ResolvedTocEntry], document_count: usize) -> Vec<Vec<String>> {
        let mut anchors_by_doc = vec![Vec::new(); document_count];
        for resolution in resolutions {
            let Some(anchor) = resolution.anchor.as_ref() else { continue };
            let Some(document_anchors) = anchors_by_doc.get_mut(resolution.document) else { continue };
            if !document_anchors.contains(anchor) {
                document_anchors.push(anchor.clone());
            }
        }
        anchors_by_doc
    }

    /// Depth-first ordinal of the entry the reader is inside, for reading
    /// position. Resolutions are stored in TOC order, so the index is the
    /// ordinal; duplicate links resolve to their first entry, matching
    /// `active_title`.
    pub(crate) fn ordinal_of_active(&self) -> Option<usize> {
        let active = self.active_link.as_ref()?;
        self.resolutions.iter().position(|resolved| &resolved.link == active)
    }

    /// Attaches annotation indices to the full publisher hierarchy, keeping
    /// only entries with annotations in their own section or a descendant.
    /// Stored ordinals use resolved TOC order, which can skip unresolved
    /// entries; those entries still survive when they parent an annotated one.
    pub(crate) fn annotation_tree(&self, ordinals: impl IntoIterator<Item = Option<usize>>) -> AnnotationTree {
        fn visit(entries: &[TocEntry], groups: &mut HashMap<&str, Vec<usize>>, next: &mut usize) -> Vec<AnnotationBranch> {
            let mut branches = Vec::new();
            for entry in entries {
                let id = *next;
                *next += 1;
                // Removing the group also ensures duplicate TOC links cannot
                // show the same annotation more than once.
                let annotations = groups.remove(entry.link.as_str()).unwrap_or_default();
                let children = visit(&entry.children, groups, next);
                if !annotations.is_empty() || !children.is_empty() {
                    branches.push(AnnotationBranch { id, title: entry.title.clone(), annotations, children });
                }
            }
            branches
        }

        let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut other = Vec::new();
        for (index, ordinal) in ordinals.into_iter().enumerate() {
            if let Some(resolved) = ordinal.and_then(|ordinal| self.resolutions.get(ordinal)) {
                groups.entry(resolved.link.as_str()).or_default().push(index);
            } else {
                other.push(index);
            }
        }
        let branches = visit(&self.entries, &mut groups, &mut 0);
        other.extend(groups.into_values().flatten());
        other.sort_unstable();
        AnnotationTree { branches, other }
    }

    /// Publisher title for a resolved TOC ordinal, including chapter groups.
    pub(crate) fn title_for_ordinal(&self, ordinal: usize) -> Option<String> {
        self.title_for_link(&self.resolutions.get(ordinal)?.link)
    }

    /// The title of the active entry, searched depth-first through the tree.
    pub(crate) fn active_title(&self) -> Option<String> {
        self.title_for_link(self.active_link.as_ref()?)
    }

    fn title_for_link(&self, active: &str) -> Option<String> {
        fn find(entries: &[TocEntry], active: &str) -> Option<String> {
            for entry in entries {
                if entry.link == active {
                    return Some(entry.title.clone());
                }
                if let Some(found) = find(&entry.children, active) {
                    return Some(found);
                }
            }
            None
        }

        find(&self.entries, active)
    }
}

fn anchor_source_positions(resolutions: &[ResolvedTocEntry], document_index: usize, source: &str, syntax: MarkupSyntax) -> Option<Vec<(Vec<usize>, usize)>> {
    let document = parse_document(source, syntax).ok()?.build_dom_checked().ok()?;
    let ids: HashMap<_, _> = document.node_ids().filter_map(|node| document.get_dom_id(node).map(|id| (id, node))).collect();
    Some(
        resolutions
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.document == document_index)
            .filter_map(|(ordinal, entry)| {
                let anchor = entry.anchor.as_deref()?;
                let node = *ids.get(anchor)?;
                let path = document.get_dom_path_to_node(node).into_iter().skip(1).map(|node| document.get_element_step(node)).collect::<Option<Vec<_>>>()?;
                Some((path, ordinal))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn entry(title: &str, children: Vec<TocEntry>) -> TocEntry {
        TocEntry { title: title.into(), link: format!("{title}.xhtml"), children }
    }

    fn toc(entries: Vec<TocEntry>) -> TocState {
        let resolutions = TocState::resolve(&entries, |link| (link != "Part.xhtml").then_some((0, None)));
        let mut toc = TocState::default();
        toc.set(entries, resolutions);
        toc
    }

    #[test]
    fn narration_skips_named_sections_without_leaking_before_or_after_them() {
        let entries = vec![TocEntry { title: "Contents".into(), link: "contents".into(), children: vec![] }, TocEntry { title: "Chapter One".into(), link: "one".into(), children: vec![] }];
        let resolutions = TocState::resolve(&entries, |link| Some((0, Some(link.into()))));
        let mut toc = TocState::default();
        toc.set(entries, resolutions);
        toc.install_source_positions(HashMap::from([(0, vec![(vec![4, 4], 0), (vec![4, 8], 1)])]), 2);
        assert!(!toc.narration_excludes_cfi("epubcfi(/6/2!/4/2/1:0)"));
        assert!(toc.narration_excludes_cfi("epubcfi(/6/2!/4/4/1:0)"));
        assert!(toc.narration_excludes_cfi("epubcfi(/6/2!/4/6/1:0)"));
        assert!(!toc.narration_excludes_cfi("epubcfi(/6/2!/4/8/1:0)"));
        assert!(!toc.narration_excludes_cfi("epubcfi(/6/4!/4/2/1:0)"));
        assert!(!toc.narration_excludes_cfi("invalid"));
    }

    #[test]
    fn narration_document_labels_do_not_exclude_following_unlisted_documents() {
        let mut toc = TocState::default();
        toc.set(vec![entry("Table of Contents", vec![])], vec![ResolvedTocEntry { link: "Table of Contents.xhtml".into(), document: 0, anchor: None }]);
        toc.install_source_positions(HashMap::new(), 2);
        assert!(toc.narration_excludes_cfi("epubcfi(/6/2!/4/2/1:0)"));
        assert!(!toc.narration_excludes_cfi("epubcfi(/6/4!/4/2/1:0)"));
    }

    #[test]
    fn sleep_chapters_group_nested_sections_in_the_same_document() {
        let entries =
            vec![TocEntry { title: "One".into(), link: "one".into(), children: vec![TocEntry { title: "Section".into(), link: "section".into(), children: vec![] }] }, TocEntry { title: "Two".into(), link: "two".into(), children: vec![] }];
        let resolutions = TocState::resolve(&entries, |link| Some((0, Some(link.into()))));
        let source = r#"<html><body><h1 id="one">One</h1><p>Body</p><h2 id="section">Section</h2><p>Body</p><h1 id="two">Two</h1><p>Body</p></body></html>"#;
        let positions = anchor_source_positions(&resolutions, 0, source, MarkupSyntax::Xml).unwrap();
        let section_path = positions.iter().find(|(_, ordinal)| *ordinal == 1).unwrap().0.iter().map(|step| format!("/{step}")).collect::<String>();
        let second_path = positions.iter().find(|(_, ordinal)| *ordinal == 2).unwrap().0.iter().map(|step| format!("/{step}")).collect::<String>();
        let section_cfi = format!("epubcfi(/6/2!{section_path}/1:0)");
        let second_cfi = format!("epubcfi(/6/2!{second_path}/1:0)");
        let mut state = TocState::default();
        state.set(entries, resolutions);
        state.install_source_positions(HashMap::from([(0, positions)]), 1);
        assert_eq!(state.chapter_for_cfi(&section_cfi), Some(0));
        assert_eq!(state.chapter_for_cfi(&second_cfi), Some(2));
    }

    #[test]
    fn document_lookup_preserves_chapter_fallbacks_without_a_source_path() {
        let entries =
            vec![TocEntry { title: "One".into(), link: "one".into(), children: vec![TocEntry { title: "Section".into(), link: "section".into(), children: vec![] }] }, TocEntry { title: "Two".into(), link: "two".into(), children: vec![] }];
        let resolutions = TocState::resolve(&entries, |link| Some((1, Some(link.into()))));
        let mut state = TocState::default();
        state.set(entries, resolutions);
        state.install_source_positions(HashMap::from([(1, vec![(vec![4, 2], 0), (vec![4, 6], 1), (vec![4, 10], 2)])]), 3);

        // A document-only target cannot distinguish its anchors, so use the
        // first entry; a precise CFI still selects the later chapter.
        assert_eq!(state.ordinal_for_document(1), Some(0));
        assert_eq!(state.chapter_for_document(1), Some(0));
        assert_eq!(state.chapter_for_cfi("epubcfi(/6/4!/4/10/1:0)"), Some(2));
        assert_eq!(state.chapter_for_document(0), None);
        assert_eq!(state.chapter_for_document(2), Some(2));
        assert_eq!(state.chapter_for_document(3), None);
        assert_eq!(state.chapter_for_document(usize::MAX), None);
    }

    #[test]
    fn annotations_keep_their_section_and_ancestors_but_prune_empty_subtrees() {
        let toc = toc(vec![entry("Part", vec![entry("Empty chapter", vec![]), entry("Chapter", vec![entry("Section", vec![]), entry("Empty section", vec![])])]), entry("Empty part", vec![])]);
        // Part is unresolved, so annotation ordinal 2 names Section rather
        // than Chapter (which has position 2 in the complete publisher tree).
        let tree = toc.annotation_tree([Some(2), Some(1), Some(2)]);
        assert!(tree.other.is_empty());
        assert_eq!(tree.branches.len(), 1);
        let part = &tree.branches[0];
        assert_eq!(part.title, "Part");
        assert!(part.annotations.is_empty());
        assert_eq!(part.children.len(), 1);
        let chapter = &part.children[0];
        assert_eq!(chapter.id, 2);
        assert_eq!(chapter.title, "Chapter");
        assert_eq!(chapter.annotations, vec![1]);
        assert_eq!(chapter.children.len(), 1);
        assert_eq!(chapter.children[0].title, "Section");
        assert_eq!(chapter.children[0].annotations, vec![0, 2]);
    }

    #[test]
    fn directly_annotated_parents_survive_without_empty_children() {
        let toc = toc(vec![entry("Chapter", vec![entry("Empty section", vec![])])]);
        let tree = toc.annotation_tree([Some(0)]);
        assert_eq!(tree.branches.len(), 1);
        assert_eq!(tree.branches[0].annotations, vec![0]);
        assert!(tree.branches[0].children.is_empty());
        assert!(toc.annotation_tree([]).branches.is_empty());
    }

    #[test]
    fn missing_locations_and_books_without_toc_keep_all_annotations() {
        let toc = toc(vec![entry("Chapter", vec![])]);
        let tree = toc.annotation_tree([None, Some(9), Some(0)]);
        assert_eq!(tree.other, vec![0, 1]);
        assert_eq!(tree.branches[0].annotations, vec![2]);
        let tree = TocState::default().annotation_tree([None, Some(0)]);
        assert!(tree.branches.is_empty());
        assert_eq!(tree.other, vec![0, 1]);
    }

    #[test]
    fn duplicate_links_show_each_annotation_once() {
        let toc = toc(vec![entry("Chapter", vec![]), entry("Chapter", vec![])]);
        let tree = toc.annotation_tree([Some(0), Some(1)]);
        assert_eq!(tree.branches.len(), 1);
        assert_eq!(tree.branches[0].annotations, vec![0, 1]);
        assert!(tree.other.is_empty());
    }

    #[test]
    fn initial_position_is_not_lost_when_toc_arrives_later() {
        let mut state = TocState::default();
        let position = SourcePosition { document: 0, path: vec![4, 6, 1], utf16_offset: Some(0) };
        assert!(!state.update_position(&position));
        let entries = vec![TocEntry { title: "Section".into(), link: "/chapter.xhtml#section".into(), children: vec![] }];
        let resolutions = TocState::resolve(&entries, |_| Some((0, Some("section".into()))));
        state.set(entries, resolutions);
        assert_eq!(state.active_link().as_deref(), Some("/chapter.xhtml#section"));
        assert_eq!(state.ordinal_of_active(), Some(0));
    }

    #[test]
    fn late_source_index_recomputes_the_retained_position() {
        let entries = vec![TocEntry { title: "First".into(), link: "chapter.xhtml#first".into(), children: vec![] }, TocEntry { title: "Second".into(), link: "chapter.xhtml#second".into(), children: vec![] }];
        let resolutions = TocState::resolve(&entries, |href| Some((0, href.split_once('#').map(|(_, anchor)| anchor.into()))));
        let positions = anchor_source_positions(&resolutions, 0, "<html><head/><body><h1 id='first'>First</h1><div id='second'/><p>Second</p></body></html>", MarkupSyntax::Xml).unwrap();
        let mut state = TocState::default();
        let position = SourcePosition { document: 0, path: vec![4, 6, 1], utf16_offset: Some(0) };
        state.update_position(&position);
        state.set(entries, resolutions);
        state.install_source_positions(HashMap::from([(0, positions)]), 1);
        assert_eq!(state.active_title().as_deref(), Some("Second"));
        assert_eq!(state.active_link().as_deref(), Some("chapter.xhtml#second"));
        assert!(!state.update_position(&position), "installing the TOC already resolved the latest page");
    }

    #[test]
    fn source_order_tracks_empty_markers_images_and_reverse_navigation() {
        let entries = ["first", "empty", "image"].map(|anchor| TocEntry { title: anchor.into(), link: format!("chapter.xhtml#{anchor}"), children: vec![] }).to_vec();
        let resolutions = TocState::resolve(&entries, |href| Some((0, href.split_once('#').map(|(_, anchor)| anchor.into()))));
        let source = "<html><head/><body><h1 id='first'>First</h1><div id='empty'/><p>After empty marker</p><div id='image'><img src='picture.svg'/></div><p>After image</p></body></html>";
        let positions = anchor_source_positions(&resolutions, 0, source, MarkupSyntax::Xml).unwrap();
        let mut state = TocState::default();
        state.set(entries, resolutions);
        state.install_source_positions(HashMap::from([(0, positions)]), 1);
        for (path, offset, expected) in [(vec![4, 2, 1], Some(0), "first"), (vec![4, 6, 1], Some(0), "empty"), (vec![4, 8, 2], None, "image"), (vec![4, 10, 1], Some(0), "image"), (vec![4, 6, 1], Some(0), "empty")] {
            state.update_position(&SourcePosition { document: 0, path, utf16_offset: offset });
            assert_eq!(state.active_title().as_deref(), Some(expected));
            assert_eq!(state.active_link(), Some(format!("chapter.xhtml#{expected}")));
        }
    }

    #[test]
    fn selected_text_is_assigned_to_its_section_at_creation() {
        let entries = vec![TocEntry { title: "Chapter".into(), link: "/chapter.xhtml#chapter".into(), children: vec![TocEntry { title: "Section".into(), link: "/chapter.xhtml#section".into(), children: vec![] }] }];
        let resolutions = TocState::resolve(&entries, |href| Some((0, href.split_once('#').map(|(_, fragment)| fragment.to_owned()))));
        let source = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><h1 id="chapter">Chapter</h1><p>Before the section</p><h2 id="section">Section</h2><p>Annotated text</p></body></html>"#;
        let positions = anchor_source_positions(&resolutions, 0, source, MarkupSyntax::Xml).unwrap();
        let mut state = TocState::default();
        state.set(entries, resolutions);
        state.install_source_positions(HashMap::from([(0, positions)]), 1);
        let ordinal = state.ordinal_for_cfi("epubcfi(/6/2!/4,/8/1:0,/8/1:9)");
        assert_eq!(ordinal, Some(1));
        let tree = state.annotation_tree([ordinal]);
        assert!(tree.other.is_empty());
        assert_eq!(tree.branches[0].children[0].annotations, vec![0]);
        assert_eq!(state.ordinal_for_cfi("epubcfi(/6/2!/4/4/1:0)"), Some(0));
        assert_eq!(state.ordinal_for_cfi("epubcfi(/6/4!/4/4/1:0)"), None);
    }

    #[test]
    fn cfi_ranges_handle_assertion_commas_and_multi_digit_paths() {
        assert_eq!(parse_source_position("epubcfi(/6/2!/4[body^,name],/12[section]/1:0,/12[section]/1:8)"), Some(SourcePosition { document: 0, path: vec![4, 12, 1], utf16_offset: Some(0) }));
        assert_eq!(parse_source_position("epubcfi(/6/2!/4/8[sec^]tion]/1:12)"), Some(SourcePosition { document: 0, path: vec![4, 8, 1], utf16_offset: Some(12) }));
        assert!(parse_source_position("epubcfi(/6/2!/4[unclosed/1:0)").is_none());
    }

    #[test]
    fn epub_archive_navigation_and_source_locations_agree_across_chapters() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let source = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><h1 id="chapter">Chapter</h1><p>Before</p><h2 id="section">Section</h2><p>Annotated text</p></body></html>"#;
        for (name, bytes) in [
            ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#),
            (
                "OEBPS/content.opf",
                r#"<package version="3.0"><manifest><item id="chapter" href="Text/chapter.xhtml" media-type="application/xhtml+xml"/><item id="next" href="Text/next.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="chapter"/><itemref idref="next"/></spine></package>"#,
            ),
            (
                "OEBPS/nav.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="Text/chapter.xhtml#chapter">Chapter</a><ol><li><a href="Text/chapter.xhtml#section">Section</a></li></ol></li><li><a href="Text/next.xhtml">Next chapter</a></li></ol></nav></body></html>"#,
            ),
            ("OEBPS/Text/chapter.xhtml", source),
            ("OEBPS/Text/next.xhtml", r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><p>Next chapter</p></body></html>"#),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(bytes.as_bytes()).unwrap();
        }
        let book = crate::book::NativeBook::from_reader(book_content::BookFormat::Epub, archive.finish().unwrap()).unwrap();
        fn entries(source: &[book_content::TocEntry]) -> Vec<TocEntry> {
            source.iter().map(|entry| TocEntry { title: entry.title.clone(), link: entry.link.clone(), children: entries(&entry.children) }).collect()
        }
        let entries = entries(book.toc());
        let resolutions = TocState::resolve(&entries, |href| TocState::resolve_publication_href(href, book.document_uris(), book.provider.as_ref()));
        assert_eq!(resolutions.len(), 3);
        let positions = TocState::source_positions(&resolutions, book.document_uris(), book.provider.as_ref());
        let mut state = TocState::default();
        state.update_position(&SourcePosition { document: 1, path: Vec::new(), utf16_offset: None });
        state.set(entries, resolutions);
        state.install_source_positions(positions, book.document_uris().len());
        assert_eq!(state.ordinal_of_active(), Some(2));
        let parsed = parse_document(source, MarkupSyntax::Xml).unwrap();
        let text = html::document::DocumentTextIndex::from_parsed(&parsed).unwrap();
        let start = text.text().find("Annotated text").unwrap();
        let position = text.source_position_in(start..start + 9).unwrap();
        let cfi = skrift_column_paginator::cfi::generate_cfi_from_source_position(0, &position);
        assert_eq!(state.ordinal_for_cfi(&cfi), Some(1));
        let tree = state.annotation_tree([state.ordinal_for_cfi(&cfi)]);
        assert!(tree.other.is_empty());
        assert_eq!(tree.branches[0].children[0].title, "Section");
    }
}
