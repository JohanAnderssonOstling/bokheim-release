//! Browser navigation, library switching, and route ownership.

mod folder_creation;
mod global_pages;
mod libraries;
mod library_session;
mod route;
pub(crate) mod shell;
mod sidebar;

use global_pages::GlobalPages;
pub(crate) use libraries::Libraries;
use library_session::LibrarySession;
pub(crate) use route::LibrarySummary;
pub use route::{GlobalRoute, LibraryRoute, Route};
pub use shell::BrowserShell;

pub(crate) type Navigate = std::rc::Rc<dyn Fn(Route, &mut gpui::Window, &mut gpui::App)>;
