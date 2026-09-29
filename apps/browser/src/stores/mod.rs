//! Shared observable state, injected downward.
//!
//! Stores outlive whichever pages read them and are owned by
//! [`BrowserRoot`](crate::root::BrowserRoot). Anything read by two siblings
//! lives here rather than being reached through a shared parent.

pub(crate) mod preferences;

pub(crate) use preferences::{CoverTextSetting, Preferences, current_cover_text};
