use super::*;
use gpui::{AppContext, Context, Modifiers, Render, TestAppContext, rems};
use gpui_component::Root;
use std::cell::Cell;

struct NumericTiles {
    width: f32,
    rem: f32,
    selections: Rc<Cell<usize>>,
    adjustments: Rc<Cell<i32>>,
}

impl Render for NumericTiles {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.rem));
        let theme = BrowserTheme::default();
        let selections = self.selections.clone();
        let decrement = self.adjustments.clone();
        let increment = self.adjustments.clone();
        let custom = reader_numeric_tile(
            "test-custom",
            "1.5 em",
            false,
            crate::settings_stepper_button("test-decrease", "−", false).debug_selector(|| "test-decrease".into()).on_click(move |_, _, cx| {
                cx.stop_propagation();
                decrement.set(decrement.get() - 1);
            }),
            crate::settings_stepper_button("test-increase", "+", false).debug_selector(|| "test-increase".into()).on_click(move |_, _, cx| {
                cx.stop_propagation();
                increment.set(increment.get() + 1);
            }),
            theme,
            move |_, _, _| selections.set(selections.get() + 1),
        );
        let presets = reader_tile_group("test-presets").child(reader_picture_tile("test-book", reader_tile_picture("icons/reader-settings/align-book.svg"), "Book", true, theme)).child(reader_picture_tile(
            "test-off",
            reader_tile_picture("icons/reader-settings/align-left.svg"),
            "Off",
            false,
            theme,
        ));
        div().w(px(self.width)).child(
            reader_tile_group("test-row").flex_wrap().debug_selector(|| "test-row".into()).child(div().flex().flex_1().min_w(rems(6.5)).child(presets)).child(div().flex().flex_1().min_w(rems(reader_numeric_tile_width_rem())).child(custom)),
        )
    }
}

#[gpui::test]
fn numeric_tiles_fit_and_stepper_clicks_do_not_select_twice(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for rem in [16.0, 24.0] {
        for width in [228.0, 272.0, 368.0] {
            let selections = Rc::new(Cell::new(0));
            let adjustments = Rc::new(Cell::new(0));
            let (_, visual) = cx.add_window_view(|window, cx| {
                let content = cx.new(|_| NumericTiles { width, rem, selections: selections.clone(), adjustments: adjustments.clone() });
                Root::new(content, window, cx)
            });
            visual.run_until_parked();
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let row = visual.debug_bounds("test-row").unwrap();
            let tile = visual.debug_bounds("test-custom").unwrap();
            let decrease = visual.debug_bounds("test-decrease").unwrap();
            let increase = visual.debug_bounds("test-increase").unwrap();
            let value = visual.debug_bounds("test-custom-value").unwrap();
            // Kobo deliberately uses the compact 2rem controls; phone controls use 3rem.
            let minimum = px(if cfg!(feature = "kobo") { 32.0 } else { 48.0 });
            for button in [decrease, increase, value] {
                assert!(button.size.width >= minimum && button.size.height >= minimum, "small touch target: {button:?}");
                assert!((button.center().y - tile.center().y).abs() < px(1.0));
            }
            assert!(tile.left() >= row.left() && tile.right() <= row.right(), "overflow at {width}px and {rem}px font: {tile:?} within {row:?}");
            assert!(decrease.right() <= value.left() && value.right() <= increase.left());
            visual.simulate_click(increase.center(), Modifiers::default());
            assert_eq!(adjustments.get(), 1);
            assert_eq!(selections.get(), 0);
            visual.simulate_click(value.center(), Modifiers::default());
            assert_eq!(selections.get(), 1);
            assert_eq!(adjustments.get(), 1);
            visual.simulate_click(decrease.center(), Modifiers::default());
            assert_eq!(adjustments.get(), 0);
            assert_eq!(selections.get(), 1);
        }
    }
}

#[cfg(feature = "kobo")]
#[test]
fn selected_kobo_numeric_tile_steppers_keep_dark_ink_on_paper() {
    use gpui::Styled;
    let theme = BrowserTheme::default();
    for selected in [false, true] {
        for label in ["−", "+"] {
            let mut button = numeric_tile_stepper(crate::settings_stepper_button("contrast-test", label, false), selected, theme);
            assert_eq!(button.style().text.color, Some(theme.text), "Kobo steppers must remain visible when Custom is selected");
        }
    }
}
