use std::{cell::Cell, rc::Rc};

use gpui::{Context, KeyDownEvent, KeyLocation, KeyUpEvent, Keystroke, Modifiers, Render, TestAppContext, VisualTestContext, Window, div, point, prelude::*, px};
use gpui_component::{Disableable, IconName};
use ui_components::{BrowserTheme, Button, NavigationIcon, compact_hierarchy_item};

struct ButtonFixture {
    disabled: bool,
    loading: bool,
    clicks: Rc<Cell<usize>>,
}

impl Render for ButtonFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let clicks = self.clicks.clone();
        Button::new("action").label("Action").icon(IconName::Check).w(px(140.0)).h(px(40.0)).disabled(self.disabled).loading(self.loading).debug_selector(|| "action".into()).on_click(move |_, _, _| clicks.set(clicks.get() + 1))
    }
}

fn activate_from_keyboard(visual: &mut VisualTestContext) {
    for key in ["enter", "space"] {
        let keystroke = Keystroke::parse(key).unwrap();
        visual.simulate_event(KeyDownEvent { keystroke: keystroke.clone(), key_location: KeyLocation::Standard, is_held: false, prefer_character_input: false });
        visual.simulate_event(KeyUpEvent { keystroke, key_location: KeyLocation::Standard });
    }
}

#[gpui::test]
fn flattened_button_preserves_keyboard_activation_and_unavailable_states(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let clicks = Rc::new(Cell::new(0));
    let (view, visual) = cx.add_window_view(|_, _| ButtonFixture { disabled: false, loading: false, clicks: clicks.clone() });
    let button = visual.debug_bounds("action").expect("button bounds");
    assert_eq!(button.size.width, px(140.0));
    assert_eq!(button.size.height, px(40.0));
    visual.simulate_click(button.center(), Modifiers::default());
    assert_eq!(clicks.get(), 1);
    visual.update(|window, cx| window.focus_next(cx));
    visual.update(|window, cx| {
        let _ = window.draw(cx);
    });
    activate_from_keyboard(visual);
    assert_eq!(clicks.get(), 3);
    for (disabled, loading) in [(true, false), (false, true)] {
        view.update(visual, |view, cx| {
            view.disabled = disabled;
            view.loading = loading;
            cx.notify();
        });
        visual.simulate_click(button.center(), Modifiers::default());
        activate_from_keyboard(visual);
        assert_eq!(clicks.get(), 3, "unavailable buttons must not activate");
    }
}

struct ChipFixture {
    opens: Rc<Cell<usize>>,
    downloads: Rc<Cell<usize>>,
}

impl Render for ChipFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let downloads = self.downloads.clone();
        let opens = self.opens.clone();
        let download = Button::new("download").label("↓").ghost().size(px(28.0)).p_0().debug_selector(|| "download".into()).on_click(move |_, _, cx| {
            cx.stop_propagation();
            downloads.set(downloads.get() + 1);
        });
        div().w(px(180.0)).h(px(40.0)).child(
            compact_hierarchy_item("chip", NavigationIcon::Folder, "A folder with a long name", Some("Library/Parent folder".into()), Some(42), Some(download.into_any_element()), false, false, false, BrowserTheme::default())
                .debug_selector(|| "chip".into())
                .on_click(move |_, _, _| opens.set(opens.get() + 1)),
        )
    }
}

#[gpui::test]
fn hierarchy_download_keeps_its_own_hit_target_after_flattening(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let opens = Rc::new(Cell::new(0));
    let downloads = Rc::new(Cell::new(0));
    let (_, visual) = cx.add_window_view(|_, _| ChipFixture { opens: opens.clone(), downloads: downloads.clone() });
    let chip = visual.debug_bounds("chip").expect("chip bounds");
    let download = visual.debug_bounds("download").expect("download bounds");
    assert_eq!(chip.size.width, px(180.0));
    assert_eq!(chip.size.height, px(40.0));
    assert!(chip.contains(&download.center()));
    visual.simulate_click(download.center(), Modifiers::default());
    assert_eq!(downloads.get(), 1);
    assert_eq!(opens.get(), 0);
    visual.simulate_click(point(chip.left() + px(50.0), chip.center().y), Modifiers::default());
    assert_eq!(opens.get(), 1);
}
