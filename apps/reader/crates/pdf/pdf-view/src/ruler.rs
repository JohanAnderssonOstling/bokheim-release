//! Reading guides anchored in PDF source coordinates, independent of selection.

use super::*;
use gpui::{black, canvas};
use pdf_reader_core::PageTextGeometry;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReadingRulerStyle {
    Band,
    Focus,
    Underline,
    #[default]
    #[serde(other)]
    Off,
}

#[derive(Clone, Copy, Debug)]
struct RulerAnchor {
    page: usize,
    character: Option<usize>,
    page_y: f32,
}

#[derive(Clone, Default)]
pub(super) struct ReadingRuler {
    style: ReadingRulerStyle,
    lines: usize,
    accent: Option<Hsla>,
    anchor: Cell<Option<RulerAnchor>>,
    edge: Cell<Option<bool>>,
    from_end: Cell<bool>,
    navigation: Cell<u64>,
}

#[derive(Clone, Debug)]
pub(super) struct TextLine {
    characters: Range<usize>,
    bounds: NormalizedRect,
    region: usize,
}

#[derive(Clone, Debug)]
struct VisibleLine {
    anchor: RulerAnchor,
    characters: Range<usize>,
    region: (usize, usize, usize),
    bounds: Bounds<Pixels>,
    column: Bounds<Pixels>,
}

/// Retain source order, including separate text columns and table cells.
pub(super) fn text_lines(geometry: &PageTextGeometry) -> Arc<[TextLine]> {
    let mut lines = Vec::<TextLine>::new();
    let mut interrupted = false;
    for character in geometry.characters() {
        if character.character().is_control() {
            interrupted = true;
            continue;
        }
        if character.character().is_whitespace() || character.bounds().height() <= 0.0 || character.bounds().width() <= 0.0 {
            continue;
        }
        let rect = character.bounds();
        let merge = lines.last().is_some_and(|line| {
            let overlap = line.bounds.bottom().min(rect.bottom()) - line.bounds.top().max(rect.top());
            let gap = (rect.left() - line.bounds.right()).max(line.bounds.left() - rect.right());
            !interrupted && overlap > line.bounds.height().min(rect.height()) * 0.3 && gap <= 0.05
        });
        if merge {
            if let Some(line) = lines.last_mut() {
                line.bounds = line.bounds.union(rect);
                line.characters.end = character.source_index() + 1;
            }
        } else {
            let region = lines.last().map_or(0, |line| line.region + usize::from(rect.top() < line.bounds.bottom() - line.bounds.height() * 0.3));
            lines.push(TextLine { characters: character.source_index()..character.source_index() + 1, bounds: rect, region });
        }
        interrupted = false;
    }
    lines.into()
}

impl PdfView {
    pub fn set_reading_ruler(&mut self, style: ReadingRulerStyle, lines: usize, accent: Hsla, cx: &mut Context<Self>) {
        let lines = lines.clamp(1, 5);
        if self.ruler.style == style && self.ruler.lines == lines && self.ruler.accent == Some(accent) {
            return;
        }
        self.ruler.style = style;
        self.ruler.lines = lines;
        self.ruler.accent = Some(accent);
        self.page_turn = None;
        cx.notify();
    }

    /// Returns false when disabled, leaving ordinary line navigation available.
    pub fn step_reading_ruler(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        if self.ruler.style == ReadingRulerStyle::Off {
            return false;
        }
        self.finish_scroll_animation(cx);
        self.page_turn = None;
        self.swipe_preview.clear();
        let lines = self.visible_ruler_lines();
        if lines.is_empty() {
            return true;
        }
        let next = self.ruler_index(&lines).and_then(|index| {
            let group = line_group(&lines, index, self.ruler.lines, self.ruler.from_end.get());
            if forward { (group.end < lines.len()).then_some(group.end) } else { group.start.checked_sub(1) }
        });
        if let Some(next) = next {
            self.ruler.from_end.set(!forward);
            self.ruler.anchor.set(Some(lines[next].anchor));
            self.ruler.edge.set(None);
        } else if (forward && self.can_next_page()) || (!forward && self.can_previous_page()) {
            self.turn_page(forward, cx);
            self.ruler.edge.set(Some(forward));
            self.ruler.anchor.set(None);
            self.ruler.from_end.set(!forward);
            self.ruler.navigation.set(self.navigation_revision);
        }
        cx.notify();
        true
    }

    pub(super) fn ruler_page_turned(&mut self, forward: bool, moved: bool) {
        if moved {
            self.ruler.anchor.set(None);
            self.ruler.edge.set(Some(forward));
            self.ruler.from_end.set(!forward);
        }
        self.ruler.navigation.set(self.navigation_revision);
    }

    fn ruler_index(&self, lines: &[VisibleLine]) -> Option<usize> {
        if self.ruler.navigation.get() != self.navigation_revision {
            self.ruler.anchor.set(None);
            self.ruler.from_end.set(false);
            self.ruler.edge.set(Some(true));
            self.ruler.navigation.set(self.navigation_revision);
        }
        if lines.is_empty() {
            return None;
        }
        if let Some(forward) = self.ruler.edge.get() {
            return Some(if forward { 0 } else { lines.len() - 1 });
        }
        let anchor = self.ruler.anchor.get();
        Some(anchor.and_then(|anchor| lines.iter().position(|line| same_anchor(anchor, line))).unwrap_or(0))
    }

    fn visible_ruler_lines(&self) -> Vec<VisibleLine> {
        let slots = page_slots(self.available_width, &self.screen_column_widths(), self.right_to_left, self.page_gap);
        let mut lines = Vec::new();
        let viewport = Bounds::new(point(px(0.0), px(0.0)), size(px(self.available_width), px(self.flow_column_height())));
        let base = self.flow_base_index();
        // A partially scrolled viewport may also contain the following stream.
        for (slot, &(left, width)) in slots.iter().enumerate() {
            for display in screen_column_stream(base, slot, self.display_column_count().unwrap_or(0)).take(if self.zoom_mode == PdfZoomMode::FitHeight { 1 } else { 2 }) {
                for column in columns_for_display(self.fit_width_plan.columns(), display) {
                    let top = (display - base - slot) as f32 * self.flow_column_height() + self.column_offset_within_display(*column) - self.flow_remainder();
                    let column_bounds = Bounds::new(point(px(left + ((width - column.display_width()) * 0.5).max(0.0)), px(top)), size(px(column.display_width()), px(column.display_height())));
                    let Some((_, page)) = self.pages.iter().find(|(index, _)| *index == column.page_index()) else { continue };
                    if page.ruler_lines.is_empty() {
                        // Scans have no text metrics. A 24px guide can still be
                        // advanced manually without pretending to perform OCR.
                        let height = column.display_height();
                        let count = (height / 24.0).ceil().max(1.0) as usize;
                        for index in 0..count {
                            let y = index as f32 * 24.0;
                            let rect = NormalizedRect::new(0.0, y / height, 1.0, 24.0f32.min(height - y) / height);
                            let bounds = normalized_bounds(rect, column_bounds);
                            if bounds.top() >= viewport.top() && bounds.bottom() <= viewport.bottom() {
                                let page_y = column.column_point_to_page((0.0, rect.top())).1;
                                let page_y = page.page_crop.top() + page_y * page.page_crop.height();
                                lines.push(VisibleLine { anchor: RulerAnchor { page: column.page_index(), character: None, page_y }, characters: 0..0, region: (slot, column.page_index(), 0), bounds, column: column_bounds });
                            }
                        }
                    } else {
                        for line in page.ruler_lines.iter() {
                            let center = line.bounds.top() + line.bounds.height() * 0.5;
                            if center < column.page_rect().top() || center >= column.page_rect().bottom() {
                                continue;
                            }
                            let Some(rect) = column.page_rect_to_column(line.bounds) else { continue };
                            let bounds = normalized_bounds(rect, column_bounds);
                            if bounds.top() < viewport.top() || bounds.bottom() > viewport.bottom() {
                                continue;
                            }
                            let page_y = page.page_crop.top() + line.bounds.top() * page.page_crop.height();
                            lines.push(VisibleLine {
                                anchor: RulerAnchor { page: column.page_index(), character: Some(line.characters.start), page_y },
                                characters: line.characters.clone(),
                                region: (slot, column.page_index(), line.region),
                                bounds,
                                column: column_bounds,
                            });
                        }
                    }
                }
            }
        }
        lines
    }

    pub(super) fn prepare_reading_ruler(&self) -> Vec<(Bounds<Pixels>, Hsla)> {
        if self.ruler.style == ReadingRulerStyle::Off {
            return Vec::new();
        }
        let lines = self.visible_ruler_lines();
        let Some(index) = self.ruler_index(&lines) else { return Vec::new() };
        self.ruler.anchor.set(Some(lines[index].anchor));
        self.ruler.edge.set(None);
        let band = line_group(&lines, index, self.ruler.lines, self.ruler.from_end.get()).fold(lines[index].bounds, |band, index| band.union(&lines[index].bounds));
        let accent = self.ruler.accent.unwrap_or_else(black);
        match self.ruler.style {
            ReadingRulerStyle::Off => Vec::new(),
            ReadingRulerStyle::Band => vec![(band, accent.opacity(0.22))],
            ReadingRulerStyle::Underline => {
                vec![(Bounds::new(band.origin, size(band.size.width, px(2.0))), accent.opacity(0.9)), (Bounds::new(point(band.left(), band.bottom() - px(2.0)), size(band.size.width, px(2.0))), accent.opacity(0.9))]
            }
            ReadingRulerStyle::Focus => {
                let column = lines[index].column;
                let band = Bounds::new(point(column.left(), band.top()), size(column.size.width, band.size.height));
                focus_mask(band, size(px(self.available_width), px(self.flow_column_height()))).into_iter().map(|bounds| (bounds, black().opacity(0.55))).collect()
            }
        }
    }

    /// A source anchor survives resizing, zooming, and margin cropping.
    pub(super) fn preserve_reading_ruler(&mut self, cx: &mut Context<Self>) {
        if self.ruler.style == ReadingRulerStyle::Off || self.ruler.navigation.get() != self.navigation_revision {
            return;
        }
        let Some(anchor) = self.ruler.anchor.get() else { return };
        if self.visible_ruler_lines().iter().any(|line| same_anchor(anchor, line)) {
            return;
        }
        let revision = self.navigation_revision;
        self.reveal_annotation(anchor.page, NormalizedRect::new(0.0, anchor.page_y, 1.0, 0.001), cx);
        self.navigation_revision = revision;
    }
}

fn same_anchor(anchor: RulerAnchor, line: &VisibleLine) -> bool {
    anchor.page == line.anchor.page && anchor.character.map_or_else(|| (anchor.page_y - line.anchor.page_y).abs() < 0.001, |character| line.characters.contains(&character))
}

fn line_group(lines: &[VisibleLine], index: usize, count: usize, from_end: bool) -> Range<usize> {
    let region = lines[index].region;
    let start = (0..index).rev().find(|&other| lines[other].region != region).map_or(0, |other| other + 1);
    let end = (index + 1..lines.len()).find(|&other| lines[other].region != region).unwrap_or(lines.len());
    if from_end { (index + 1).saturating_sub(count.max(1)).max(start)..index + 1 } else { index..index.saturating_add(count.max(1)).min(end) }
}

fn focus_mask(band: Bounds<Pixels>, viewport: gpui::Size<Pixels>) -> Vec<Bounds<Pixels>> {
    [
        Bounds::new(point(px(0.0), px(0.0)), size(viewport.width, band.top())),
        Bounds::new(point(px(0.0), band.bottom()), size(viewport.width, viewport.height - band.bottom())),
        Bounds::new(point(px(0.0), band.top()), size(band.left(), band.size.height)),
        Bounds::new(point(band.right(), band.top()), size(viewport.width - band.right(), band.size.height)),
    ]
    .into_iter()
    .filter(|bounds| bounds.size.width > px(0.0) && bounds.size.height > px(0.0))
    .collect()
}

pub(super) fn overlay(quads: &[(Bounds<Pixels>, Hsla)]) -> impl IntoElement {
    let quads = quads.to_vec();
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            for (rect, color) in &quads {
                window.paint_quad(fill(Bounds::new(bounds.origin + rect.origin, rect.size), *color));
            }
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, rgb};
    use pdf_reader_core::PageTextGeometry;
    use pdf_view_common::TextCharacter;

    #[test]
    fn source_lines_keep_columns_and_control_boundaries() {
        let geometry = PageTextGeometry::new([
            TextCharacter::new(0, 'A', NormalizedRect::new(0.1, 0.1, 0.02, 0.02)),
            TextCharacter::new(1, 'B', NormalizedRect::new(0.12, 0.1, 0.02, 0.02)),
            TextCharacter::new(2, '\n', NormalizedRect::new(0.0, 0.0, 0.0, 0.0)),
            TextCharacter::new(3, 'C', NormalizedRect::new(0.1, 0.2, 0.02, 0.02)),
            TextCharacter::new(4, 'D', NormalizedRect::new(0.6, 0.1, 0.02, 0.02)),
            TextCharacter::new(5, 'E', NormalizedRect::new(0.6, 0.2, 0.02, 0.02)),
        ]);
        let lines = text_lines(&geometry);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].characters, 0..2);
        assert_eq!(lines[0].region, lines[1].region);
        assert_ne!(lines[1].region, lines[2].region);
        assert_eq!(lines[2].region, lines[3].region);
    }

    fn install_lines(view: &mut PdfView) {
        super::super::tests::install_test_pages(view);
        for (_, page) in &mut view.pages {
            let mut copy = DisplayPage {
                raster: PageImage { image: page.raster.image.clone(), width: page.raster.width, height: page.raster.height, byte_len: page.raster.byte_len },
                text_layout: page.text_layout.clone(),
                ruler_lines: Arc::from([]),
                raster_crop: page.raster_crop,
                page_crop: page.page_crop,
            };
            copy.ruler_lines = (0..6).map(|index| TextLine { characters: index * 10..index * 10 + 5, bounds: NormalizedRect::new(0.1, 0.1 + index as f32 * 0.1, 0.8, 0.03), region: 0 }).collect::<Vec<_>>().into();
            *page = Arc::new(copy);
        }
    }

    #[gpui::test]
    fn styles_and_line_groups_follow_source_order_without_touching_selection(cx: &mut TestAppContext) {
        let (view, cx) = super::super::tests::setup(cx);
        view.update(cx, |view, cx| {
            install_lines(view);
            let revision = view.navigation_revision;
            let geometry = PageTextGeometry::new([TextCharacter::new(0, 'A', NormalizedRect::new(0.1, 0.1, 0.1, 0.1))]);
            let selection = Arc::new(geometry.select((0.12, 0.12), (0.18, 0.18)).unwrap());
            view.selection = Some(selection.clone());
            assert!(!view.step_reading_ruler(true, cx));
            view.set_reading_ruler(ReadingRulerStyle::Band, 2, rgb(0x466853).into(), cx);
            let first = view.prepare_reading_ruler();
            assert_eq!(first.len(), 1);
            let original = view.ruler.anchor.get().unwrap();
            view.step_reading_ruler(true, cx);
            let second = view.prepare_reading_ruler();
            assert_ne!(first, second);
            assert_eq!(view.ruler.anchor.get().unwrap().character, Some(20));
            view.step_reading_ruler(false, cx);
            assert_eq!(view.prepare_reading_ruler(), first);
            assert_eq!(view.ruler.anchor.get().unwrap().character, Some(10));
            assert_eq!(view.ruler.anchor.get().unwrap().page, original.page);
            assert!(Arc::ptr_eq(view.selection.as_ref().unwrap(), &selection));
            assert_eq!(view.navigation_revision, revision);
            view.set_reading_ruler(ReadingRulerStyle::Underline, 2, rgb(0x466853).into(), cx);
            let rules = view.prepare_reading_ruler();
            assert_eq!(rules.len(), 2);
            assert!(rules.iter().all(|(bounds, _)| bounds.size.height == px(2.0)));
            view.set_reading_ruler(ReadingRulerStyle::Focus, 2, rgb(0x466853).into(), cx);
            let masks = view.prepare_reading_ruler();
            let active = view.visible_ruler_lines();
            let index = view.ruler_index(&active).unwrap();
            assert!(masks.len() >= 3, "focus dims the other display columns too");
            assert!(masks.iter().all(|(bounds, _)| !bounds.contains(&active[index].bounds.center())));
            view.set_reading_ruler(ReadingRulerStyle::Off, 2, rgb(0x466853).into(), cx);
            assert!(view.prepare_reading_ruler().is_empty());
        });
    }

    #[gpui::test]
    fn ruler_finishes_a_column_then_turns_and_resumes_from_the_last_line(cx: &mut TestAppContext) {
        let (view, cx) = super::super::tests::setup(cx);
        view.update(cx, |view, cx| {
            install_lines(view);
            view.set_reading_ruler(ReadingRulerStyle::Band, 5, rgb(0x466853).into(), cx);
            let lines = view.visible_ruler_lines();
            let last = lines.last().unwrap().anchor;
            view.ruler.anchor.set(Some(last));
            view.ruler.edge.set(None);
            view.ruler.navigation.set(view.navigation_revision);
            let previous = view.flow_offset;
            view.step_reading_ruler(true, cx);
            assert!(view.flow_offset > previous);
            install_lines(view);
            view.prepare_reading_ruler();
            view.step_reading_ruler(false, cx);
            install_lines(view);
            view.prepare_reading_ruler();
            assert_eq!(view.flow_offset, previous);
            assert_eq!(view.ruler.anchor.get().unwrap().character, last.character);
            assert_eq!(view.ruler.anchor.get().unwrap().page, last.page);
            let current_lines = view.visible_ruler_lines();
            let index = view.ruler_index(&current_lines).unwrap();
            let group = line_group(&current_lines, index, 5, view.ruler.from_end.get());
            assert_eq!(group.len(), 5);
        });
    }

    #[gpui::test]
    #[cfg(not(target_arch = "wasm32"))]
    fn resizing_keeps_the_ruler_passage_visible_instead_of_resetting_to_the_first_line(cx: &mut TestAppContext) {
        let session = PdfDocumentSession::from_bytes("ruler-reflow", include_bytes!("../../../../../../shared/pdf-reader-core/tests/fixtures/cover-annotations.pdf").to_vec()).unwrap();
        let (view, cx) = super::super::tests::setup(cx);
        view.update(cx, |view, cx| {
            view.info = Some(session.info().clone());
            view.trim_margins = false;
            view.zoom_mode = PdfZoomMode::FitWidth { columns: 2 };
            view.page_geometry.insert(PageGeometryKey { page_index: 0, trim_margins: false }, PdfPageGeometry { crop: NormalizedRect::new(0.0, 0.0, 1.0, 1.0), break_avoidance: Arc::from([]) });
            view.set_available_size(600.0, 200.0, cx);
            install_lines(view);
            view.set_reading_ruler(ReadingRulerStyle::Band, 1, rgb(0x466853).into(), cx);
            view.prepare_reading_ruler();
            for _ in 0..4 {
                view.step_reading_ruler(true, cx);
                view.prepare_reading_ruler();
            }
            let anchor = view.ruler.anchor.get().unwrap();
            assert_eq!(anchor.character, Some(40));
            view.set_available_size(1200.0, 200.0, cx);
            install_lines(view);
            assert!(!view.prepare_reading_ruler().is_empty());
            assert_eq!(view.ruler.anchor.get().unwrap().character, anchor.character);
            assert_eq!(view.ruler.anchor.get().unwrap().page, anchor.page);
        });
    }

    #[gpui::test]
    fn scan_guides_and_rtl_spreads_work_without_a_text_layer(cx: &mut TestAppContext) {
        let (view, cx) = super::super::tests::setup(cx);
        view.update(cx, |view, cx| {
            super::super::tests::install_test_pages(view);
            view.right_to_left = true;
            view.set_reading_ruler(ReadingRulerStyle::Band, 3, rgb(0x466853).into(), cx);
            let lines = view.visible_ruler_lines();
            assert!(!lines.is_empty());
            assert!(lines[0].column.left() > lines.last().unwrap().column.left());
            let first = view.prepare_reading_ruler();
            assert!((f32::from(first[0].0.size.height) - 72.0).abs() < 0.001);
            view.step_reading_ruler(true, cx);
            let second = view.prepare_reading_ruler();
            assert!((f32::from(second[0].0.top() - first[0].0.top()) - 72.0).abs() < 0.001);
            view.step_reading_ruler(false, cx);
            assert_eq!(view.prepare_reading_ruler(), first);
        });
    }
}
