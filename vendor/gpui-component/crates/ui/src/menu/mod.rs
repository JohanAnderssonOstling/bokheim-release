use gpui::App;

mod app_menu_bar;
mod choice_row;
mod context_menu;
mod dropdown_menu;
mod menu_item;
mod popup_menu;

pub use app_menu_bar::AppMenuBar;
pub use choice_row::{ChoiceRowColors, choice_row, choice_row_checkmark};
pub use context_menu::{ContextMenu, ContextMenuExt, ContextMenuState, OpenContextMenu};
pub use dropdown_menu::DropdownMenu;
pub use popup_menu::{PopupMenu, PopupMenuItem};

pub(crate) fn init(cx: &mut App) {
    context_menu::init(cx);
    app_menu_bar::init(cx);
    popup_menu::init(cx);
}
