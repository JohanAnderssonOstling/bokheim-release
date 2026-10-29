//! Mechanics every reader surface needs, written once.
//!
//! EPUB and PDF differ in how they present a book, but share persistence,
//! annotation, marks, and chrome mechanics. Those parts live here so a fix
//! lands in one reader instead of three.

pub(crate) mod annotation_editor;
pub(crate) mod annotation_tree;
#[cfg(feature = "kobo")]
pub(crate) mod brightness;
pub(crate) mod chrome;
pub(crate) mod context_sheet;
pub(crate) mod input;
pub(crate) mod keyboard_settings;
mod layout;
pub(crate) mod marks;
pub(crate) mod page;
pub(crate) mod persistence;
pub(crate) mod reading_ruler;
mod screenshot;
pub(crate) mod search;
pub(crate) mod settings_navigation;
pub(crate) mod sidebar_list;
pub(crate) mod state_panel;
pub(crate) mod theme_picker;
pub(crate) mod toolbar;
#[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
pub(crate) mod tts;
