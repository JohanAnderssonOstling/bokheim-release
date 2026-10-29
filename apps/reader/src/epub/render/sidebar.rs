//! EPUB contents, annotation, and settings sidebar tabs.

use crate::invalidation::notify;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, IntoElement, Window, div};
use gpui_component::input::Input;
use library_backend::Annotation;
use std::rc::Rc;
use ui_components as components;

use crate::epub::ReaderView;
use crate::invalidation::Component;
use crate::shell::annotation_tree::AnnotationTreeTarget;
use crate::shell::sidebar_list;
use crate::shell::state_panel::{self, AnnotationSidebarTarget, SidebarTab, TocActivation};
use crate::shell::toolbar::SidebarTarget;

impl AnnotationSidebarTarget for ReaderView {
    fn select_sidebar_annotation(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.annotation_popup_anchor = None;
        self.select_annotation(id, window, cx);
    }

    fn go_to_annotation(&mut self, annotation: &Annotation, cx: &mut Context<Self>) {
        self.navigate_to_cfi(annotation.anchor.epub_cfi_value().unwrap_or_default().to_owned(), cx);
    }

    fn delete_sidebar_annotation(&mut self, id: String, cx: &mut Context<Self>) {
        self.delete_annotation(id, cx);
    }
}

impl AnnotationTreeTarget for ReaderView {
    fn set_annotation_branch_expanded(&mut self, id: usize, expanded: bool, cx: &mut Context<Self>) {
        self.annotation_tree.set_expanded(id, expanded);
        notify(cx, Component::ReaderShell, "annotation_branch_toggled");
    }
}

impl ReaderView {
    fn render_toc_body(&self, entity: Entity<Self>, theme: components::BrowserTheme, cx: &App) -> AnyElement {
        let active_link = self.toc.active_link();
        let reset_entity = entity.clone();
        let reset_read: state_panel::TocReadReset = Rc::new(move |target, _, cx| {
            reset_entity.update(cx, |this, cx| {
                this.reading_positions.reset_chapter_read_status(target.to_owned());
                this.toc.reset_chapter_read_status(target);
                cx.notify();
            });
        });
        let toc = state_panel::reader_toc_tree(
            &self.toc_tree,
            &entity,
            active_link.as_deref(),
            self.toc.read_status.completed.iter().cloned().collect(),
            Some(reset_read),
            self.settings_navigation.keyboard_visible(),
            theme,
            cx,
            |link, activation, window, this: &mut Self, cx| {
                this.navigate_to(link.to_owned(), cx);
                if activation == TocActivation::Pointer {
                    this.focus_sidebar_body(window, cx);
                    #[cfg(feature = "kobo")]
                    {
                        this.sidebar.close(cx);
                        this.focus_document(window, cx);
                        notify(cx, Component::ReaderShell, "toc_navigated");
                    }
                }
            },
        );
        let reset = components::topbar_action_button("reset-chapter-read-status", "Reset read status", gpui_component::IconName::Undo2, theme).on_click(move |_, _, cx| {
            entity.update(cx, |this, cx| {
                this.reading_positions.reset_read_status();
                this.toc.read_status.reset();
                cx.notify();
            });
        });
        components::reader_toc(theme).child(gpui::div().flex_none().p(gpui::px(components::SPACE_SM)).child(reset)).child(toc).into_any_element()
    }

    fn render_annotations_body(&mut self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> AnyElement {
        let mut tree = self.toc.annotation_tree(self.marks.annotations().iter().map(|annotation| annotation.toc_ordinal.and_then(|ordinal| usize::try_from(ordinal).ok())));
        let query = self.annotation_filter_input.read(cx).value();
        tree.filter(self.marks.annotations(), &query);
        self.annotation_tree.set_filter(&query);
        let selected_id = self.marks.selected_id();
        let selected_index = self.marks.annotations().iter().position(|annotation| Some(annotation.id.as_str()) == selected_id);
        self.annotation_tree.reveal_selected(&tree, selected_id, selected_index);
        let empty_message = if self.marks.annotations().is_empty() { "No notes yet. Select text, then choose Annotation to add a note." } else { "No annotations match your filter." };
        self.annotation_tree.prepare_list(&tree, self.marks.annotations(), selected_id, empty_message);
        let rows = sidebar_list::panel("epub-annotation-list", &cx.entity(), &self.annotation_tree.list.state, theme, |this, row, target, theme, _| {
            this.annotation_tree.render_row(target, row, this.marks.annotations(), this.marks.selected_id(), theme)
        });
        let filter = components::reader_search_bar(theme)
            .child(components::reader_search_box(theme).debug_selector(|| "reader-annotation-filter".into()).child(Input::new(&self.annotation_filter_input).appearance(false).cleanable(true).size_full()));
        div().size_full().min_h_0().min_w_0().flex().flex_col().child(div().flex_none().child(filter)).child(rows).into_any_element()
    }

    /// The selected panel's body; the shell supplies its rail and header.
    pub(super) fn render_sidebar(&mut self, active: SidebarTab, entity: Entity<Self>, theme: components::BrowserTheme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match active {
            SidebarTab::Contents => self.render_toc_body(entity, theme, cx),
            SidebarTab::Notes => self.render_annotations_body(theme, cx),
            SidebarTab::Search => self.render_search(theme, window, cx),
            SidebarTab::Settings => self.render_settings_panel(window, cx),
        }
    }
}
