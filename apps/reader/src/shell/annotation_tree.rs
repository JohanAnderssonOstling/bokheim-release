//! A filtered contents hierarchy with annotation cards beneath each entry.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, div};
use ui_components as components;

use super::sidebar_list::SidebarList;
use super::state_panel::{AnnotationRow, AnnotationSidebarTarget, render_annotation_row};
use library_backend::Annotation;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AnnotationTree {
    pub(crate) branches: Vec<AnnotationBranch>,
    /// Annotation indices whose stored locations no longer match the TOC.
    pub(crate) other: Vec<usize>,
}

impl AnnotationTree {
    /// Prune cards and empty chapters without changing annotation indices or
    /// publisher branch IDs, so card actions still address the original list.
    pub(crate) fn filter(&mut self, annotations: &[Annotation], query: &str) {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        let matches: Vec<_> = annotations.iter().map(|annotation| annotation.exact_text.to_lowercase().contains(&query) || annotation.note.to_lowercase().contains(&query)).collect();
        fn retain(branches: &mut Vec<AnnotationBranch>, matches: &[bool]) {
            branches.retain_mut(|branch| {
                branch.annotations.retain(|&index| matches[index]);
                retain(&mut branch.children, matches);
                !branch.annotations.is_empty() || !branch.children.is_empty()
            });
        }
        retain(&mut self.branches, &matches);
        self.other.retain(|&index| matches[index]);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AnnotationBranch {
    /// Position in the full publisher tree, before pruning empty entries.
    pub(crate) id: usize,
    pub(crate) title: String,
    pub(crate) annotations: Vec<usize>,
    pub(crate) children: Vec<AnnotationBranch>,
}

impl AnnotationBranch {
    fn contains(&self, annotation: usize) -> bool {
        self.annotations.contains(&annotation) || self.children.iter().any(|child| child.contains(annotation))
    }
}

#[derive(Default)]
pub(crate) struct AnnotationTreeState {
    collapsed: HashSet<usize>,
    filter: String,
    filtered_collapsed: HashSet<usize>,
    selected_id: Option<String>,
    pub(crate) list: SidebarList<AnnotationTreeRow>,
}

impl AnnotationTreeState {
    /// Show matches immediately, while retaining the unfiltered disclosure state.
    pub(crate) fn set_filter(&mut self, query: &str) {
        let query = query.trim().to_lowercase();
        if self.filter != query {
            self.filter = query;
            self.filtered_collapsed.clear();
        }
    }

    fn collapsed_mut(&mut self) -> &mut HashSet<usize> {
        if self.filter.is_empty() { &mut self.collapsed } else { &mut self.filtered_collapsed }
    }

    pub(crate) fn expanded(&self, id: usize) -> bool {
        let collapsed = if self.filter.is_empty() { &self.collapsed } else { &self.filtered_collapsed };
        !collapsed.contains(&id)
    }

    pub(crate) fn set_expanded(&mut self, id: usize, expanded: bool) {
        if expanded {
            self.collapsed_mut().remove(&id);
        } else {
            self.collapsed_mut().insert(id);
        }
    }

    /// Selecting a passage in the document reveals its card, even if its
    /// ancestors were folded. A subsequent manual collapse stays collapsed.
    pub(crate) fn reveal_selected(&mut self, tree: &AnnotationTree, selected_id: Option<&str>, selected_index: Option<usize>) {
        if self.selected_id.as_deref() == selected_id {
            return;
        }
        self.selected_id = selected_id.map(str::to_owned);
        let Some(index) = selected_index else { return };
        fn reveal(branches: &[AnnotationBranch], index: usize, collapsed: &mut HashSet<usize>) {
            for branch in branches {
                if branch.contains(index) {
                    collapsed.remove(&branch.id);
                    reveal(&branch.children, index, collapsed);
                }
            }
        }
        reveal(&tree.branches, index, self.collapsed_mut());
    }
}

pub(crate) trait AnnotationTreeTarget: AnnotationSidebarTarget {
    fn set_annotation_branch_expanded(&mut self, id: usize, expanded: bool, cx: &mut Context<Self>);
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AnnotationTreeRow {
    Branch { id: usize, title: String, depth: usize, expanded: bool },
    Annotation(AnnotationRow),
}

impl AnnotationTreeState {
    pub(crate) fn prepare_list(&mut self, tree: &AnnotationTree, annotations: &[Annotation], selected_id: Option<&str>, no_annotations: &'static str) {
        let rows = annotation_tree_rows(tree, self, annotations, no_annotations);
        let selected_row = selected_id.and_then(|selected| rows.iter().position(|row| matches!(row, AnnotationTreeRow::Annotation(row) if row.annotation_id() == Some(selected))));
        self.list.set_rows(rows);
        self.list.reveal_selected(selected_id, selected_row);
    }

    pub(crate) fn render_row<C: AnnotationTreeTarget>(&self, target: &Entity<C>, index: usize, annotations: &[Annotation], selected_id: Option<&str>, theme: components::BrowserTheme) -> AnyElement {
        match self.list.rows.get(index) {
            Some(AnnotationTreeRow::Annotation(row)) => render_annotation_row(target, row, annotations, selected_id, &self.filter, theme),
            Some(AnnotationTreeRow::Branch { id, title, depth, expanded }) => {
                let (id, expanded) = (*id, *expanded);
                let row = components::ContentsRow { depth: *depth, has_children: true, expanded, ..Default::default() };
                let click_target = target.downgrade();
                let key_target = target.downgrade();
                components::contents_row(("reader-annotation-branch", id), row, Some(components::contents_disclosure(("reader-annotation-disclosure", id), row, theme)), title.clone(), None, None, theme)
                    .flex_none()
                    .debug_selector(move || format!("reader-annotation-branch-{id}"))
                    .focusable()
                    .tab_stop(true)
                    .on_click(move |_, _, cx| {
                        let _ = click_target.update(cx, |this, cx| this.set_annotation_branch_expanded(id, !expanded, cx));
                    })
                    .on_key_down(move |event, _, cx| {
                        if event.keystroke.modifiers != gpui::Modifiers::default() {
                            return;
                        }
                        let expanded = match event.keystroke.key.as_str() {
                            "left" => false,
                            "right" => true,
                            "enter" | "space" => !expanded,
                            _ => return,
                        };
                        let _ = key_target.update(cx, |this, cx| this.set_annotation_branch_expanded(id, expanded, cx));
                        cx.stop_propagation();
                    })
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}

/// Flatten only the expanded hierarchy. Cards are constructed later by the
/// list callback, using the original annotation indices.
fn annotation_tree_rows(tree: &AnnotationTree, state: &AnnotationTreeState, annotations: &[Annotation], no_annotations: &'static str) -> Vec<AnnotationTreeRow> {
    fn visit(branches: &[AnnotationBranch], state: &AnnotationTreeState, annotations: &[Annotation], depth: usize, rows: &mut Vec<AnnotationTreeRow>) {
        for branch in branches {
            let expanded = state.expanded(branch.id);
            rows.push(AnnotationTreeRow::Branch { id: branch.id, title: branch.title.clone(), depth, expanded });
            if expanded {
                rows.extend(branch.annotations.iter().map(|&index| AnnotationTreeRow::Annotation(AnnotationRow::card(index, depth + 1, &annotations[index]))));
                visit(&branch.children, state, annotations, depth + 1, rows);
            }
        }
    }
    let mut rows = Vec::new();
    if tree.branches.is_empty() && tree.other.is_empty() {
        return vec![AnnotationTreeRow::Annotation(AnnotationRow::Empty(no_annotations))];
    }
    visit(&tree.branches, state, annotations, 0, &mut rows);
    if !tree.other.is_empty() {
        rows.push(AnnotationTreeRow::Annotation(AnnotationRow::Heading("Other notes".into())));
        rows.extend(tree.other.iter().map(|&index| AnnotationTreeRow::Annotation(AnnotationRow::card(index, 0, &annotations[index]))));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::{AnnotationAnchor, AnnotationStyle};
    use content_address::BookId;
    use gpui::{Render, TestAppContext, VisualTestContext, Window, px};

    struct TreeTest {
        tree: AnnotationTree,
        state: AnnotationTreeState,
        annotations: Vec<Annotation>,
        selected_id: Option<String>,
        navigated_id: Option<String>,
        deleted_id: Option<String>,
    }

    impl AnnotationSidebarTarget for TreeTest {
        fn select_sidebar_annotation(&mut self, id: String, _: &mut Window, cx: &mut Context<Self>) {
            self.selected_id = Some(id);
            cx.notify();
        }

        fn go_to_annotation(&mut self, annotation: &Annotation, cx: &mut Context<Self>) {
            self.navigated_id = Some(annotation.id.clone());
            cx.notify();
        }

        fn delete_sidebar_annotation(&mut self, id: String, cx: &mut Context<Self>) {
            self.deleted_id = Some(id);
            cx.notify();
        }
    }

    impl AnnotationTreeTarget for TreeTest {
        fn set_annotation_branch_expanded(&mut self, id: usize, expanded: bool, cx: &mut Context<Self>) {
            self.state.set_expanded(id, expanded);
            cx.notify();
        }
    }

    impl Render for TreeTest {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.state.prepare_list(&self.tree, &self.annotations, self.selected_id.as_deref(), "No notes yet. Select text, then choose Annotation to add a note.");
            div().w(px(320.0)).h_full().child(super::super::sidebar_list::panel("test-annotation-list", &cx.entity(), &self.state.list.state, components::browser_theme(cx), |this, row, target, theme, _| {
                this.state.render_row(target, row, &this.annotations, this.selected_id.as_deref(), theme)
            }))
        }
    }

    fn annotation(id: &str) -> Annotation {
        Annotation {
            id: id.into(),
            content_hash: BookId::from_bytes([0; 32]),
            anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2!/4/2/1:0)".to_owned()),
            exact_text: "A passage with enough text to occupy several lines in the annotation sidebar.".into(),
            style: AnnotationStyle::Highlight,
            color: components::READER_ANNOTATION_DEFAULT_COLOR,
            note: "The reader's note".into(),
            created_at: 0,
            toc_ordinal: None,
            progress: None,
            modified_at: 0,
            deleted: false,
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn tree_expands_and_collapses_without_clipping_cards_and_cards_still_navigate(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| TreeTest {
            tree: AnnotationTree {
                branches: vec![AnnotationBranch { id: 0, title: "Part".into(), annotations: vec![0], children: vec![AnnotationBranch { id: 1, title: "Chapter".into(), annotations: vec![1], children: vec![] }] }],
                other: vec![],
            },
            state: AnnotationTreeState::default(),
            annotations: vec![annotation("part-note"), annotation("chapter-note")],
            selected_id: None,
            navigated_id: None,
            deleted_id: None,
        });
        draw(cx);
        let parent = cx.debug_bounds("reader-annotation-branch-0").unwrap();
        let child = cx.debug_bounds("reader-annotation-branch-1").unwrap();
        let parent_card = cx.debug_bounds("reader-annotation-card-0").unwrap();
        let child_card = cx.debug_bounds("reader-annotation-card-1").unwrap();
        assert!(parent_card.top() >= parent.bottom());
        assert!(child.top() >= parent_card.bottom());
        assert!(child_card.top() >= child.bottom());
        assert!(child_card.left() > parent_card.left());
        assert!(child_card.size.height > px(60.0));
        cx.simulate_click(parent.center(), Default::default());
        draw(cx);
        assert!(cx.debug_bounds("reader-annotation-branch-1").is_none());
        assert!(cx.debug_bounds("reader-annotation-card-0").is_none());
        assert!(cx.debug_bounds("reader-annotation-card-1").is_none());
        cx.simulate_keystrokes("right");
        draw(cx);
        let child_card = cx.debug_bounds("reader-annotation-card-1").unwrap();
        // The upper portion holds the quote; the actions live below it.
        cx.simulate_click(child_card.origin + gpui::point(px(30.0), px(20.0)), Default::default());
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_id.as_deref(), Some("chapter-note"));
            assert_eq!(view.navigated_id.as_deref(), Some("chapter-note"));
        });
        view.update(cx, |view, _| view.navigated_id = None);
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| assert_eq!(view.navigated_id.as_deref(), Some("chapter-note")));
        draw(cx);
        let delete = cx.debug_bounds("reader-annotation-delete-0").unwrap();
        cx.simulate_click(delete.center(), Default::default());
        view.read_with(cx, |view, _| {
            assert_eq!(view.deleted_id.as_deref(), Some("part-note"));
            assert_eq!(view.selected_id.as_deref(), Some("chapter-note"));
            assert_eq!(view.navigated_id.as_deref(), Some("chapter-note"));
        });
    }

    #[gpui::test]
    fn large_annotation_list_renders_nearby_cards_and_reveals_selection(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| TreeTest {
            tree: AnnotationTree { branches: vec![], other: (0..10_000).collect() },
            state: AnnotationTreeState::default(),
            annotations: (0..10_000).map(|index| annotation(&format!("note-{index}"))).collect(),
            selected_id: None,
            navigated_id: None,
            deleted_id: None,
        });
        draw(cx);
        assert!(cx.debug_bounds("reader-annotation-card-0").is_some());
        assert!(cx.debug_bounds("reader-annotation-card-100").is_none());
        assert!(cx.debug_bounds("reader-annotation-card-9999").is_none());
        let first_height = cx.debug_bounds("reader-annotation-card-0").unwrap().size.height;
        view.update(cx, |view, cx| {
            view.annotations[9_999].note = "A long note that wraps over several lines. ".repeat(30);
            view.annotations[9_999].modified_at += 1;
            view.selected_id = Some("note-9999".into());
            cx.notify();
        });
        draw(cx);
        let card = cx.debug_bounds("reader-annotation-card-9999").unwrap();
        assert!(card.size.height > first_height, "variable-height notes must not be clipped to the estimate");
        assert!(cx.debug_bounds("reader-annotation-card-0").is_none());
        // The selected card can be taller than the viewport; click its visible
        // quote rather than its geometric center.
        let visible_top = card.top().max(px(0.0));
        cx.simulate_click(gpui::point(card.left() + px(30.0), visible_top + px(20.0)), Default::default());
        view.read_with(cx, |view, _| assert_eq!(view.navigated_id.as_deref(), Some("note-9999")));
        view.update(cx, |view, cx| {
            view.annotations[9_999].note.clear();
            view.annotations[9_999].modified_at += 1;
            cx.notify();
        });
        draw(cx);
        assert!(cx.debug_bounds("reader-annotation-card-9999").unwrap().size.height < card.size.height, "editing a note must invalidate its measured height");
    }

    #[test]
    fn selecting_an_annotation_reveals_only_its_ancestors_and_preserves_manual_collapse() {
        let tree = AnnotationTree {
            branches: vec![
                AnnotationBranch { id: 0, title: "Part".into(), annotations: vec![], children: vec![AnnotationBranch { id: 1, title: "Chapter".into(), annotations: vec![0], children: vec![] }] },
                AnnotationBranch { id: 2, title: "Another part".into(), annotations: vec![1], children: vec![] },
            ],
            other: vec![],
        };
        let mut state = AnnotationTreeState::default();
        for id in 0..3 {
            state.set_expanded(id, false);
        }
        state.reveal_selected(&tree, Some("first"), Some(0));
        assert!(state.expanded(0));
        assert!(state.expanded(1));
        assert!(!state.expanded(2));
        state.set_expanded(0, false);
        state.reveal_selected(&tree, Some("first"), Some(0));
        assert!(!state.expanded(0));
        state.reveal_selected(&tree, None, None);
        state.reveal_selected(&tree, Some("first"), Some(0));
        assert!(state.expanded(0));
    }

    #[gpui::test]
    fn filtered_cards_navigate_and_delete_the_original_annotation(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| {
            let annotations = vec![annotation("first"), Annotation { note: "Find this note".into(), ..annotation("second") }];
            let mut tree = AnnotationTree { branches: vec![AnnotationBranch { id: 3, title: "Chapter".into(), annotations: vec![0, 1], children: vec![] }], other: vec![] };
            tree.filter(&annotations, "find");
            TreeTest { tree, state: AnnotationTreeState::default(), annotations, selected_id: None, navigated_id: None, deleted_id: None }
        });
        draw(cx);
        assert!(cx.debug_bounds("reader-annotation-card-0").is_none());
        let card = cx.debug_bounds("reader-annotation-card-1").unwrap();
        cx.simulate_click(card.origin + gpui::point(px(30.0), px(20.0)), Default::default());
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_id.as_deref(), Some("second"));
            assert_eq!(view.navigated_id.as_deref(), Some("second"));
        });
        draw(cx);
        let delete = cx.debug_bounds("reader-annotation-delete-1").unwrap();
        cx.simulate_click(delete.center(), Default::default());
        view.read_with(cx, |view, _| assert_eq!(view.deleted_id.as_deref(), Some("second")));
    }

    #[test]
    fn flat_annotation_groups_preserve_original_indices_without_copying_cards() {
        use crate::shell::state_panel::reader_state_rows;

        let annotations = vec![annotation("first"), annotation("second"), annotation("third")];
        let rows = reader_state_rows(&annotations, "No notes", |annotation| if annotation.id == "second" { ("other".into(), "Other notes".into()) } else { ("page".into(), "Page 1".into()) });
        assert_eq!(
            rows,
            vec![AnnotationRow::Heading("Page 1".into()), AnnotationRow::card(0, 0, &annotations[0]), AnnotationRow::card(2, 0, &annotations[2]), AnnotationRow::Heading("Other notes".into()), AnnotationRow::card(1, 0, &annotations[1])]
        );
        assert_eq!(reader_state_rows(&[], "No notes", |_| unreachable!()), [AnnotationRow::Empty("No notes")]);
    }

    #[test]
    fn filtering_matches_quotes_and_notes_and_preserves_original_indices() {
        let annotations = vec![
            Annotation { exact_text: "Unrelated passage".into(), note: String::new(), ..annotation("first") },
            Annotation { exact_text: "CAFÉ society".into(), note: String::new(), ..annotation("quote") },
            Annotation { exact_text: "Another passage".into(), note: "Visit the café".into(), ..annotation("note") },
        ];
        let make_tree = || AnnotationTree {
            branches: vec![
                AnnotationBranch { id: 0, title: "Part".into(), annotations: vec![0], children: vec![AnnotationBranch { id: 1, title: "Chapter".into(), annotations: vec![1], children: vec![] }] },
                AnnotationBranch { id: 2, title: "Unmatched chapter".into(), annotations: vec![0], children: vec![] },
            ],
            other: vec![0, 2],
        };
        let mut tree = make_tree();
        tree.filter(&annotations, "  Café  ");
        assert_eq!(tree.branches.len(), 1);
        assert_eq!(tree.branches[0].id, 0);
        assert!(tree.branches[0].annotations.is_empty());
        assert_eq!(tree.branches[0].children[0].id, 1);
        assert_eq!(tree.branches[0].children[0].annotations, [1]);
        assert_eq!(tree.other, [2]);

        tree.filter(&annotations, "missing");
        assert_eq!(tree, AnnotationTree::default());
        let mut tree = make_tree();
        tree.filter(&annotations, "   ");
        assert_eq!(tree, make_tree());
    }

    #[test]
    fn filtering_reveals_matches_and_restores_unfiltered_disclosure_state() {
        let mut state = AnnotationTreeState::default();
        state.set_expanded(1, false);
        state.set_filter("passage");
        assert!(state.expanded(1));
        state.set_expanded(2, false);
        state.set_filter(" PASSAGE ");
        assert!(!state.expanded(2));
        state.set_filter("another");
        assert!(state.expanded(2));
        state.set_filter("");
        assert!(!state.expanded(1));
        assert!(state.expanded(2));
    }
}
