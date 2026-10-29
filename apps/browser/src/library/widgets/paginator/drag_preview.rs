//! Drag appearances reuse the displayed children at their measured sizes.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Bounds, ElementId, Entity, IntoElement, Pixels, SharedString, Window, div, point};
use ui_components as components;

use super::{Paginator, PaginatorElement, PaginatorPage, PaginatorState};

#[derive(Clone)]
pub(crate) struct PaginatorDragPreview {
    state: Rc<PaginatorState>,
    children: Vec<(usize, Bounds<Pixels>)>,
    bounds: Bounds<Pixels>,
}

impl PaginatorState {
    pub(crate) fn drag_preview(&self, id: &SharedString) -> Option<PaginatorDragPreview> {
        if self.preview {
            return None;
        }
        let children: Vec<_> = self
            .target_ids(id)
            .iter()
            .filter_map(|id| {
                let index = *self.data.indices.get(id)?;
                self.bounds.iter().find(|(child, _)| *child == index).copied()
            })
            .collect();
        let (_, first) = children.first()?;
        let mut bounds = *first;
        for (_, child) in &children {
            bounds = Bounds::from_corners(point(bounds.left().min(child.left()), bounds.top().min(child.top())), point(bounds.right().max(child.right()), bounds.bottom().max(child.bottom())));
        }
        let mut state = self.clone();
        state.preview = true;
        Some(PaginatorDragPreview { state: Rc::new(state), children, bounds })
    }
}

impl PaginatorDragPreview {
    pub(crate) fn render(&self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        div().relative().w(self.bounds.size.width).h(self.bounds.size.height).children(
            self.children
                .iter()
                .map(|(index, bounds)| {
                    let child = &self.state.data.children[*index];
                    let shadow = self.state.data.group_index_for_child(*index).is_some_and(|group| self.state.data.groups[group].policy.item_shadow);
                    let theme = components::browser_theme(cx);
                    let content = (child.render)(&self.state, window, cx);
                    let content = if child.owns_cell { content } else { div().size_full().overflow_hidden().child(content).into_any_element() };
                    div()
                        .absolute()
                        .left(bounds.left() - self.bounds.left())
                        .top(bounds.top() - self.bounds.top())
                        .w(bounds.size.width)
                        .h(bounds.size.height)
                        .when(shadow, |cell| cell.bg(theme.page_bg).shadow(components::section_card_shadow(theme)))
                        .child(content)
                })
                .collect::<Vec<_>>(),
        )
    }
}

impl Paginator {
    /// Render the current contents without paging, selecting, or reporting new
    /// hit bounds to the source paginator.
    pub(crate) fn preview(view: Entity<Self>, window: &Window, cx: &App) -> impl IntoElement {
        let paginator = view.read(cx);
        let state = Rc::new(PaginatorState { cursor: paginator.cursor, selection: paginator.selection.clone(), preview: true, data: paginator.data.clone(), ..Default::default() });
        let element = PaginatorElement::new(
            ElementId::from(format!("{}-drag-preview", paginator.id)),
            view.downgrade(),
            paginator,
            PaginatorPage { reported_visible_page: paginator.visible_page, child_offset: paginator.child_offset, direction: paginator.direction, prefetched: None, forced_page: paginator.visible_page },
            window.rem_size(),
            state,
        )
        .passive();
        div().size_full().min_w_0().min_h_0().child(element)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PaginatorChild, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy};
    use super::*;
    use gpui::{Context, Render, TestAppContext, VisualTestContext, px, rgb, size};
    use std::collections::BTreeSet;

    #[test]
    fn selection_preview_preserves_item_sizes_and_their_relative_positions() {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(120.0)), PaginatorSizing::pixels(px(180.0)), px(0.0));
        let paginator = Paginator::new("preview", [PaginatorGroup::new(policy, (0..3).map(|index| PaginatorChild::new(|_, _, _| div().into_any_element()).with_id(index.to_string())))]);
        let state = PaginatorState {
            selection: BTreeSet::from([0, 2]),
            data: paginator.data.clone(),
            bounds: Rc::new(vec![
                (0, Bounds::new(point(px(100.0), px(200.0)), size(px(120.0), px(180.0)))),
                (1, Bounds::new(point(px(240.0), px(200.0)), size(px(120.0), px(180.0)))),
                (2, Bounds::new(point(px(400.0), px(240.0)), size(px(160.0), px(200.0)))),
            ]),
            ..Default::default()
        };
        let preview = state.drag_preview(&"0".into()).unwrap();
        assert_eq!(preview.bounds, Bounds::new(point(px(100.0), px(200.0)), size(px(460.0), px(240.0))));
        assert_eq!(preview.children, vec![state.bounds[0], state.bounds[2]]);
        assert!(preview.state.preview);
        assert!(preview.state.drag_preview(&"0".into()).is_none(), "the dragged copy must not recursively create more drag previews");
        assert!(!state.preview);
        let unselected = state.drag_preview(&"1".into()).unwrap();
        assert_eq!(unselected.bounds.size, size(px(120.0), px(180.0)));
        assert_eq!(unselected.children.len(), 1);
    }

    struct PreviewFixture(PaginatorDragPreview);

    impl Render for PreviewFixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.render(window, cx)
        }
    }

    #[gpui::test]
    fn preview_cells_preserve_clipping_with_and_without_outer_shadows(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for owns_cell in [false, true] {
            for shadow in [false, true] {
                let child = PaginatorChild::new(move |state, _, _| {
                    assert!(state.preview);
                    div()
                        .id("preview-cell")
                        .debug_selector(|| "preview-cell".into())
                        .size_full()
                        .relative()
                        .when(owns_cell, |cell| cell.overflow_hidden())
                        .child(div().absolute().left(px(80.0)).top_0().w(px(80.0)).h(px(40.0)).bg(rgb(0xfe0102)))
                        .into_any_element()
                })
                .with_id("book");
                let child = if owns_cell { child.with_own_cell() } else { child };
                let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(80.0)), px(0.0));
                let policy = if shadow { policy.with_item_shadow() } else { policy };
                let paginator = Paginator::new("preview-clipping", [PaginatorGroup::new(policy, [child])]);
                let state = PaginatorState { data: paginator.data.clone(), bounds: Rc::new(vec![(0, Bounds::new(point(px(30.0), px(20.0)), size(px(100.0), px(80.0))))]), ..Default::default() };
                let preview = state.drag_preview(&"book".into()).unwrap();
                let window = cx.open_window(size(px(300.0), px(200.0)), |_, _| PreviewFixture(preview));
                let visual = &mut VisualTestContext::from_window(window.into(), cx);
                visual.run_until_parked();
                let bounds = visual.debug_bounds("preview-cell").unwrap();
                assert_eq!(bounds, Bounds::new(point(px(0.0), px(0.0)), size(px(100.0), px(80.0))));
                visual.update(|window, cx| {
                    let quads = window.painted_quads();
                    let overflow = quads.iter().find(|quad| quad.background == rgb(0xfe0102).into()).unwrap();
                    assert_eq!(overflow.content_mask.bounds, bounds.scale(window.scale_factor()));
                    let background = components::browser_theme(cx).page_bg;
                    assert_eq!(quads.iter().any(|quad| quad.background == background.into() && quad.bounds == bounds.scale(window.scale_factor())), shadow);
                });
            }
        }
    }
}
