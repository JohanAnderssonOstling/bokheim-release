//! Parsed EPUB, MOBI and FB2 text containers, and CBZ page archives.
//! The crate has no renderer dependency; consumers adapt its prepared resources.

mod book;
pub mod comic;
mod epub;
mod fb2;
#[cfg(test)]
mod format_tests;
mod inspection;
mod metadata;
pub mod mobi;
mod profile;
mod resource_path;
mod resources;
mod source;
mod toc;
mod xml;

pub use book::{Book, BookFormat, ReadingBook};
pub use epub::opening::EpubOpeningPlan;
pub use inspection::DocumentInspection;
pub use metadata::{CreatorRole, DeclaredMetadata, IdentifierScheme, MetadataCredit, MetadataDate, MetadataIdentifier, MetadataIdentifierType, MetadataProperty, MetadataSubject, MetadataTitle};
pub use profile::{PreparationProfile, PreparationStage, PreparationTiming, ResourcePreparation, ResourceReadTiming};
pub use resource_path::resolve_resource_href;
pub use resources::PreparedResources;
pub use source::BookReader;
pub use toc::TocEntry;
