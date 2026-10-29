//! Search controls shared by every reader surface.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Div, ElementId, Entity, Focusable, FontWeight, HighlightStyle, Stateful, StyledText, Window, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable, IconName};
use skrift_column_paginator::SearchOptions;
use std::ops::Range;
use ui_components as components;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// Match navigation driven by the common search controls.
pub(crate) trait SearchBarTarget: Sized + 'static {
    fn search_navigate(&mut self, direction: i8, cx: &mut Context<Self>);
    fn search_position(&self) -> (usize, usize) {
        (0, 0)
    }
}

/// Match count and navigation sit in the panel header, before its close button.
pub(crate) fn header_controls<C: SearchBarTarget>(owner: &C, target: &Entity<C>, theme: components::BrowserTheme) -> AnyElement {
    let previous_target = target.clone();
    let next_target = target.clone();
    let (current, total) = owner.search_position();
    let controls = div()
        .min_w_0()
        .flex_none()
        .debug_selector(|| "reader-search-controls".into())
        .flex()
        .items_center()
        .gap(px(components::SPACE_XXS))
        .child(components::reader_match_count(format!("{current} of {total}"), theme))
        .child(
            components::outlined_icon_button("reader-search-previous", "Previous match", IconName::ChevronLeft)
                .disabled(total == 0)
                .tooltip(components::shortcut_tooltip_with_alternatives("Previous match", &["shift-enter", "up"]))
                .on_click(move |_, _, cx| {
                    previous_target.update(cx, |this, cx| this.search_navigate(-1, cx));
                }),
        )
        .child(components::outlined_icon_button("reader-search-next", "Next match", IconName::ChevronRight).disabled(total == 0).tooltip(components::shortcut_tooltip_with_alternatives("Next match", &["enter", "down"])).on_click(
            move |_, _, cx| {
                next_target.update(cx, |this, cx| this.search_navigate(1, cx));
            },
        ));
    controls.into_any_element()
}

/// The query occupies its own full-width row below the panel header.
pub(crate) fn search_bar(input: &Entity<InputState>, theme: components::BrowserTheme, window: &mut Window, cx: &mut App) -> AnyElement {
    let focus = input.read(cx).focus_handle(cx);
    // This state lives only while the query is rendered, so reopening the
    // panel focuses it again without stealing focus on subsequent renders.
    window.use_keyed_state(("reader-search-autofocus", input.entity_id()), cx, move |window, cx| focus.focus(window, cx));
    let query = components::reader_search_box(theme).debug_selector(|| "reader-search-query".into()).key_context("ReaderSearch").child(Input::new(input).appearance(false).cleanable(false).size_full().font_weight(FontWeight::NORMAL));
    components::reader_search_bar(theme).debug_selector(|| "reader-search-row".into()).child(query).into_any_element()
}

/// The query stays visible while the excerpts scroll below it.
#[cfg(test)]
pub(crate) fn panel(bar: AnyElement, rows: Vec<AnyElement>, theme: components::BrowserTheme) -> AnyElement {
    div().size_full().min_h_0().min_w_0().flex().flex_col().child(bar).child(components::reader_state_panel(theme).children(rows)).into_any_element()
}

pub(crate) fn result_row(id: impl Into<ElementId>, excerpt: String, query: &str, options: SearchOptions, selected: bool, theme: components::BrowserTheme) -> Stateful<Div> {
    let excerpt = highlighted_text(excerpt, query, options);
    components::reader_state_card(id, selected, theme)
        .flex_none()
        .cursor_pointer()
        .child(div().w_full().min_w_0().whitespace_normal().line_clamp(3).text_ellipsis().text_size(gpui::rems(components::TEXT_SM)).line_height(gpui::relative(1.35)).text_color(theme.text).child(excerpt))
}

pub(crate) fn highlighted_text(text: String, query: &str, options: SearchOptions) -> StyledText {
    if query.is_empty() {
        return StyledText::new(text);
    }
    let highlights = match_ranges(&text, query, options).into_iter().map(|range| (range, HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() }));
    StyledText::new(text).with_highlights(highlights)
}

/// Keep byte offsets into the original excerpt when folding case or accents.
fn match_ranges(excerpt: &str, query: &str, options: SearchOptions) -> Vec<Range<usize>> {
    let normalize = |text: &str| {
        let mut chars = Vec::new();
        for (start, character) in text.char_indices() {
            let range = start..start + character.len_utf8();
            let parts: Vec<_> = if options.match_diacritics { vec![character] } else { character.to_string().nfd().filter(|part| !is_combining_mark(*part)).collect() };
            for part in parts {
                if options.match_case {
                    chars.push((part, range.clone()));
                } else {
                    chars.extend(part.to_lowercase().map(|folded| (folded, range.clone())));
                }
            }
        }
        chars
    };
    let text = normalize(excerpt);
    let query = normalize(query);
    if query.is_empty() {
        return Vec::new();
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (start, window) in text.windows(query.len()).enumerate() {
        if !window.iter().map(|(character, _)| character).eq(query.iter().map(|(character, _)| character)) {
            continue;
        }
        if options.whole_word && (start.checked_sub(1).is_some_and(|index| text[index].0.is_alphanumeric()) || text.get(start + query.len()).is_some_and(|(character, _)| character.is_alphanumeric())) {
            continue;
        }
        let range = window[0].1.start..window[window.len() - 1].1.end;
        if let Some(previous) = ranges.last_mut().filter(|previous| previous.end >= range.start) {
            previous.end = previous.end.max(range.end);
        } else {
            ranges.push(range);
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_matches_preserve_unicode_byte_offsets_and_merge_overlaps() {
        let options = SearchOptions::default();
        let excerpt = "Élan CAFÉ cafe";
        let ranges = match_ranges(excerpt, "cafe", options);
        assert_eq!(ranges.iter().map(|range| &excerpt[range.clone()]).collect::<Vec<_>>(), ["CAFÉ", "cafe"]);
        assert_eq!(match_ranges("banana", "ana", options), [1..6]);
        assert!(match_ranges(excerpt, "", options).is_empty());
        assert!(match_ranges(excerpt, "a longer query than the excerpt", options).is_empty());
    }

    #[test]
    fn excerpt_matches_respect_search_options() {
        let options = SearchOptions::default();
        assert_eq!(match_ranges("CAFÉ cafe cafeteria", "cafe", SearchOptions { whole_word: true, ..options }), [0..5, 6..10]);
        assert_eq!(match_ranges("CAFÉ cafe", "cafe", SearchOptions { match_diacritics: true, ..options }), [6..10]);
        assert_eq!(match_ranges("CAFE cafe", "cafe", SearchOptions { match_case: true, ..options }), [5..9]);
    }
}
