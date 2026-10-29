use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui::{
    App, AppContext, AvailableSpace, Bounds, Context, FontStyle, FontWeight, HeadlessAppContext, IntoElement, Pixels, PlatformTextSystem, Point, Render, RenderGlyphParams, SUBPIXEL_VARIANTS_X, StyledText, Window, canvas, div, prelude::*,
    px, relative, size,
};
use gpui_component::Root;
use gpui_wgpu::CosmicTextSystem;
use ui_components::{BOOK_CARD_COVER_CONTENT_GAP, BOOK_CARD_PADDING, BOOK_CARD_TITLE_HEIGHT_REM, BrowserTheme, Button, book_card, book_card_title_row, reader_setting_checkbox, settings_choice_button};

type Probe = Rc<dyn Fn(&mut Window, &mut App)>;

struct Fixture(Probe);

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let probe = self.0.clone();
        div().size_full().child(canvas(move |_, window, cx| probe(window, cx), |_, _, _, _| {}).size_full())
    }
}

fn run_probe(scale: f32, probe: impl Fn(&mut Window, &mut App, &CosmicTextSystem) + 'static) {
    let platform = Arc::new(CosmicTextSystem::new_without_system_fonts("NV Garamond"));
    let mut app = HeadlessAppContext::with_platform_and_scale_factor(platform.clone(), Arc::new(ui_components::Assets), || None, scale);
    app.update(|cx| {
        gpui_component::init(cx);
        ui_components::register_fonts(cx).unwrap();
        ui_components::register_cjk_fonts(cx).unwrap();
    });
    let ran = Rc::new(Cell::new(false));
    let called = ran.clone();
    let probe: Probe = Rc::new(move |window, cx| {
        called.set(true);
        probe(window, cx, &platform);
    });
    let window = app
        .open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|_| Fixture(probe));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    app.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .unwrap();
    assert!(ran.get());
}

fn measure(element: impl IntoElement, width: Pixels, window: &mut Window, cx: &mut App) -> Pixels {
    element.into_any_element().layout_as_root(size(AvailableSpace::Definite(width), AvailableSpace::MaxContent), window, cx).height
}

#[test]
fn intrinsic_labels_contain_real_glyph_ink_at_multiple_sizes_and_scales() {
    for scale in [1., 1.25, 2.] {
        run_probe(scale, move |window, cx, platform| {
            assert!(window.text_style().line_height_auto);
            for family in ["NV Garamond", "Raleway"] {
                for font_size in [14., 16., 24., 40.] {
                    for style in [FontStyle::Normal, FontStyle::Italic] {
                        for weight in [FontWeight::NORMAL, FontWeight::BOLD] {
                            for label in ["Reading ruler", "gjpqy ÅÉgj", "g\u{323} Å\u{301} 漢字", "Reading ruler labels wrap onto several lines"] {
                                let text = StyledText::new(label);
                                let layout = text.layout().clone();
                                let height = measure(div().font_family(family).when(style == FontStyle::Italic, |text| text.italic()).font_weight(weight).text_size(px(font_size)).child(text), px(140.), window, cx);
                                let line_height = layout.line_height();
                                assert!(height >= line_height);
                                for line in layout.line_layouts() {
                                    let baseline = (line_height - line.ascent() - line.descent()) / 2. + line.ascent();
                                    for run in line.runs() {
                                        for glyph in &run.glyphs {
                                            for variant in 0..SUBPIXEL_VARIANTS_X {
                                                let bounds = platform
                                                    .glyph_raster_bounds(&RenderGlyphParams {
                                                        font_id: run.font_id,
                                                        glyph_id: glyph.id,
                                                        font_size: px(font_size),
                                                        subpixel_variant: Point::new(variant, 0),
                                                        scale_factor: scale,
                                                        is_emoji: glyph.is_emoji,
                                                        subpixel_rendering: false,
                                                        dilation: 0,
                                                    })
                                                    .unwrap();
                                                if bounds.size.height.0 == 0 {
                                                    continue;
                                                }
                                                let top = ((f32::from(baseline + glyph.position.y) * scale).round() + bounds.origin.y.0 as f32) / scale;
                                                let bottom = top + bounds.size.height.0 as f32 / scale;
                                                assert!(top >= -0.001 && bottom <= f32::from(line_height) + 0.001, "{family} {font_size}px {style:?} {weight:?} {label:?} scale={scale}: ink={top}..{bottom}, line={line_height:?}");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

#[test]
fn controls_use_spare_height_before_growing() {
    run_probe(1., |window, cx, _| {
        window.set_rem_size(px(16.));
        let theme = BrowserTheme::default();
        let custom = Button::new("custom-height").label("Reading ruler").h(px(24.));
        assert_eq!(measure(custom, px(400.), window, cx), px(24.));
        for font_size in [16., 20., 48.] {
            let button = Button::new("intrinsic-button").label("Reading ruler").text_size(px(font_size));
            let choice = settings_choice_button("intrinsic-choice", "Reading ruler", false, theme).text_size(px(font_size));
            let checkbox = reader_setting_checkbox("intrinsic-checkbox", "Reading ruler", false, theme).text_size(px(font_size));
            for (element, minimum) in [(button.into_any_element(), 32.), (choice.into_any_element(), 32.), (checkbox.into_any_element(), 38.)] {
                let height = measure(element, px(400.), window, cx);
                if font_size < 48. {
                    assert_eq!(height, px(minimum), "{font_size}px text should consume spare height first");
                } else {
                    assert!(height > px(minimum), "large text must be able to grow the control");
                }
            }
        }
    });
}

#[test]
fn explicit_paragraph_spacing_overrides_inherited_intrinsic_height() {
    run_probe(1., |window, cx, _| {
        let height = measure(div().line_height_auto().text_size(px(16.)).child(div().line_height(relative(2.)).child("Reading ruler")), px(400.), window, cx);
        assert_eq!(height, px(32.));
    });
}

#[test]
fn fixed_size_button_samples_fill_their_uniform_squares() {
    run_probe(1., |window, cx, _| {
        for rem in [16., 24.] {
            window.set_rem_size(px(rem));
            let sample_size = Rc::new(Cell::new(size(px(0.), px(0.))));
            let measured = sample_size.clone();
            // Theme swatches fill a fixed square through the button's content
            // wrapper. Measure the painted sample, not just the outer button.
            let sample =
                div().size_full().relative().flex().items_center().justify_center().child(div().text_size(gpui::rems(1.5)).child("Aa")).child(canvas(move |bounds, _, _| measured.set(bounds.size), |_, _, _, _| {}).absolute().inset_0());
            let mut button = Button::new("uniform-swatch").ghost().size(gpui::rems(3.)).p_0().child(sample).into_any_element();
            button.prepaint_as_root(Point::default(), size(AvailableSpace::MaxContent, AvailableSpace::MaxContent), window, cx);
            assert_eq!(sample_size.get(), size(px(rem * 3.), px(rem * 3.)));
        }
    });
}

#[test]
fn card_titles_preserve_cover_gap_and_reserved_height_with_real_fonts() {
    run_probe(1., |window, cx, _| {
        let mut index = 0;
        let measure_bounds = |bounds: Rc<Cell<Bounds<Pixels>>>| canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _, _, _| {}).absolute().inset_0();
        for rem in [16., 24.] {
            window.set_rem_size(px(rem));
            for width in [120., 220.] {
                for selected in [false, true] {
                    for (title, subtitle) in [("Book", None), ("A long title that wraps across several lines", Some("A longer subtitle that also needs more than one line")), ("", None)] {
                        let cover = Rc::new(Cell::new(Bounds::default()));
                        let title_bounds = Rc::new(Cell::new(Bounds::default()));
                        let mut card = book_card(format!("title-contract-{index}"), false, selected, false, BrowserTheme::default())
                            .w(px(width))
                            .child(div().relative().w_full().h(px(60.)).flex_none().child(measure_bounds(cover.clone())))
                            .child(book_card_title_row(title, subtitle).relative().child(measure_bounds(title_bounds.clone())))
                            .into_any_element();
                        index += 1;
                        let measured = card.layout_as_root(size(AvailableSpace::Definite(px(width)), AvailableSpace::MaxContent), window, cx);
                        card.prepaint(window, cx);
                        let (cover, title) = (cover.get(), title_bounds.get());
                        assert_eq!(cover.size.height, px(60.));
                        assert_eq!(title.size, size(px(width - BOOK_CARD_PADDING * 2.), px(rem * BOOK_CARD_TITLE_HEIGHT_REM)));
                        assert_eq!(title.top() - cover.bottom(), px(BOOK_CARD_COVER_CONTENT_GAP));
                        assert_eq!(measured.height, px(BOOK_CARD_PADDING * 2. + 60. + BOOK_CARD_COVER_CONTENT_GAP + rem * BOOK_CARD_TITLE_HEIGHT_REM));
                    }
                }
            }
        }
    });
}
