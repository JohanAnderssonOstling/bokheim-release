use gpui::prelude::*;
use gpui::{Div, div, px};
use gpui_component::{Icon, IconName};

use crate::BrowserTheme;

/// Previous/next chapter: a chevron against a bar. The icon set has no
/// skip-to-start glyph.
pub fn audiobook_chapter_icon(forward: bool, theme: BrowserTheme) -> Div {
    let chevron = Icon::new(if forward { IconName::ChevronRight } else { IconName::ChevronLeft }).size(px(19.0));
    let bar = div().w(px(2.0)).h(px(17.0)).bg(theme.text_muted);
    let icon = div().flex().items_center();
    if forward { icon.child(chevron).child(bar) } else { icon.child(bar).child(chevron) }
}
