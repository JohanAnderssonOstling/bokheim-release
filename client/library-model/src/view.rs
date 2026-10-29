use crate::BookTocEntry;
use book_model::AuthorId;
use content_address::BookId;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;

type DirId = uuid::Uuid;
type LibraryId = uuid::Uuid;

/// Percentage at which a reading position is presented as finished.
pub const FINISHED_PROGRESS_THRESHOLD: f32 = 98.0;

/// Stable runtime identity for a book within one configured library.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BookLocator {
    library_id: LibraryId,
    content_hash: BookId,
}

impl BookLocator {
    pub fn new(library_id: LibraryId, content_hash: BookId) -> Self {
        Self { library_id, content_hash }
    }

    pub fn library_id(&self) -> &LibraryId {
        &self.library_id
    }

    pub fn content_hash(&self) -> BookId {
        self.content_hash
    }
}

/// A change relevant to a currently displayed library page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LibraryUpdate {
    Contents,
    Covers(Vec<BookId>),
    Download(crate::BookDownloadStatus),
    /// A queued user action failed after its initial command was accepted.
    DownloadFailed {
        content_hash: BookId,
        /// Why it failed, when known; the diagnostic stays in the backend log.
        failure: Option<operation_failure::Failure>,
    },
    CatalogImport {
        entry: CardKey,
        progress: crate::CatalogImportProgress,
    },
    Scanning(bool),
    /// Durable sync progress changed after a completed exchange.
    SyncStatusChanged,
}

/// Limits hierarchical library browsing to one broad book family.
/// `Book` intentionally means every supported reading format except PDF and
/// M4B, which have dedicated filters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryFileTypeFilter {
    #[default]
    All,
    Book,
    Pdf,
    Audiobook,
}

impl LibraryFileTypeFilter {
    pub const ALL: [Self; 4] = [Self::All, Self::Book, Self::Pdf, Self::Audiobook];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Book => "Books",
            Self::Pdf => "PDF",
            Self::Audiobook => "Audiobooks",
        }
    }

    pub fn matches_extension(self, extension: &str) -> bool {
        match self {
            Self::All => true,
            Self::Pdf => extension.eq_ignore_ascii_case("pdf"),
            Self::Audiobook => extension.eq_ignore_ascii_case("m4b") || extension.eq_ignore_ascii_case("mp3folder"),
            Self::Book => matches!(extension.to_ascii_lowercase().as_str(), "epub" | "mobi" | "azw" | "azw3" | "fb2" | "fbz" | "fb2.zip" | "cbz"),
        }
    }
}

/// Independent ordering for hierarchy chips on the shared browse page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowseChipSort {
    #[default]
    Alphabetical,
    BookCount,
    Popularity,
    /// Home's existing shelf order, offered only by Home.
    DefaultOrder,
}

impl BrowseChipSort {
    pub const ALL: [Self; 3] = [Self::Alphabetical, Self::BookCount, Self::Popularity];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "Alphabetical",
            Self::BookCount => "Book count",
            Self::Popularity => "Popularity",
            Self::DefaultOrder => "Default order",
        }
    }
}

/// Independent ordering for book cards on the shared browse page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowseBookSort {
    #[default]
    Alphabetical,
    RecentlyRead,
    Popularity,
    /// Home's existing activity order, offered only by Home.
    DefaultOrder,
}

impl BrowseBookSort {
    pub const ALL: [Self; 3] = [Self::Alphabetical, Self::RecentlyRead, Self::Popularity];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "Alphabetical",
            Self::RecentlyRead => "Recently read",
            Self::Popularity => "Popularity",
            Self::DefaultOrder => "Default order",
        }
    }
}

/// A browsable node: all books, a folder, a route in the subject hierarchy, an author,
/// a shelf of Home, a route in the public-domain catalog's subject hierarchy, an
/// author of the catalog's books, or a page of one of the user's OPDS feeds.
/// `Author(None)` is the author index, every credited name, and
/// `CatalogAuthor(None)` the catalog's; `Home(None)` is Home itself, its
/// shelves. An author or a shelf holds books and nothing below them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BrowseLocation {
    Folder(DirId),
    Subject(i64),
    Author(Option<AuthorId>),
    Home(Option<HomeShelf>),
    Books,
    Catalog(i64),
    CatalogAuthor(Option<AuthorId>),
    /// `page` is `None` for the feed's root, otherwise a page this session
    /// has visited, by its index in the session's page table.
    Opds {
        source: OpdsSourceId,
        page: Option<u32>,
    },
    /// A store's chip at the catalog root. Opening it opens the store's
    /// window; there is nothing to browse.
    Store(StoreId),
}

impl BrowseLocation {
    /// The directory this location is, when it is a folder.
    pub fn folder(self) -> Option<DirId> {
        match self {
            Self::Folder(id) => Some(id),
            _ => None,
        }
    }
}

/// Home's shelves, in the order Home shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HomeShelf {
    RecentlyRead,
    Finished,
    RecentlyAdded,
}

impl HomeShelf {
    pub fn label(self) -> &'static str {
        match self {
            Self::RecentlyRead => "Recently read",
            Self::Finished => "Finished",
            Self::RecentlyAdded => "Recently added",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::RecentlyRead => "recently-read",
            Self::Finished => "finished",
            Self::RecentlyAdded => "recently-added",
        }
    }
}

impl std::fmt::Display for BrowseLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Books => write!(formatter, "books:"),
            Self::Folder(id) => write!(formatter, "folder:{id}"),
            Self::Subject(route) => write!(formatter, "subject:{route}"),
            Self::Author(None) => write!(formatter, "author:"),
            Self::Author(Some(id)) => write!(formatter, "author:{id}"),
            Self::Home(None) => write!(formatter, "home:"),
            Self::Home(Some(shelf)) => write!(formatter, "home:{}", shelf.slug()),
            Self::Catalog(route) => write!(formatter, "catalog:{route}"),
            Self::CatalogAuthor(None) => write!(formatter, "catalog-author:"),
            Self::CatalogAuthor(Some(id)) => write!(formatter, "catalog-author:{id}"),
            Self::Opds { source, page: None } => write!(formatter, "opds:{source}"),
            Self::Opds { source, page: Some(page) } => write!(formatter, "opds:{source}:{page}"),
            Self::Store(id) => write!(formatter, "store:{id}"),
        }
    }
}

/// One directly browsable child returned by a library hierarchy query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseRow {
    /// Labels and paths are display data.
    pub id: BrowseLocation,
    pub name: String,
    /// Parent hierarchy for scoped search results; absent for direct children.
    pub path: Option<String>,
    pub book_count: usize,
    pub downloaded_book_count: usize,
}

/// One folder or subject in the displayed breadcrumb trail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsePathSegment {
    pub id: BrowseLocation,
    pub name: String,
}

/// Direct contents for one hierarchy location.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowseContents {
    pub children: Vec<BrowseRow>,
    pub books: Vec<BookCardRow>,
}

/// One panel in the shared folder and subject browse presentation.
///
/// `parent` is the folder or subject represented by the panel; its immediate
/// children and directly assigned books are rendered inside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowseSection {
    pub parent: BrowseRow,
    pub children: Vec<BrowseRow>,
    pub books: Vec<BookCardRow>,
    /// Total directly displayed books when only the first page is included.
    /// Absent for complete sections, including older browse responses.
    #[serde(default)]
    pub book_count: Option<usize>,
}

/// A bounded range of a section's books, in the active browse order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowseBookPage {
    pub books: Vec<BookCardRow>,
    pub offset: usize,
    pub total: usize,
}

/// Initial section covers include a small buffer for responsive layouts.
pub const BROWSE_SECTION_BOOK_LIMIT: usize = 8;
pub const MAX_BROWSE_SECTION_BOOK_LIMIT: usize = 128;

/// Independent windows for the outer section list and the direct book grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsePageRequest {
    pub section_offset: usize,
    pub section_limit: usize,
    pub book_offset: usize,
    pub book_limit: usize,
}

impl Default for BrowsePageRequest {
    fn default() -> Self {
        Self { section_offset: 0, section_limit: 24, book_offset: 0, book_limit: 48 }
    }
}

/// Tree navigation stays complete; flat author headings use a bounded window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowsePageState {
    pub section_ids: Vec<BrowseLocation>,
    pub section_offset: usize,
    pub book_offset: usize,
    pub book_total: usize,
    #[serde(default)]
    pub headings: Option<BrowseHeadingState>,
    /// Selection, ordering and state revision shared by every continuation.
    #[serde(default)]
    pub result_revision: Option<String>,
}

impl BrowsePageState {
    pub fn heading_count(&self) -> usize {
        self.headings.as_ref().map_or(self.section_ids.len(), |headings| headings.total)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseHeadingState {
    pub total: usize,
    /// Detect reordered or changed headings before merging another window.
    pub revision: String,
    pub cards: bool,
}

/// Number of visible library books tagged with a base language.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryLanguageCount {
    pub language: String,
    pub book_count: u32,
}

/// Number of visible library books in one broad format category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFormatCount {
    pub format: LibraryFileTypeFilter,
    pub book_count: u32,
}

/// Synthetic root of the subject browse hierarchy.
pub const ROOT_SUBJECT_ROUTE_ID: i64 = 0;

/// Typed input shared by folder and subject browsing implementations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryBrowseQuery {
    pub location: BrowseLocation,
    pub search: String,
    pub file_types: Vec<LibraryFileTypeFilter>,
    pub chip_sort: BrowseChipSort,
    pub book_sort: BrowseBookSort,
    pub languages: Vec<String>,
    /// Drops books at or past [`FINISHED_PROGRESS_THRESHOLD`].
    ///
    /// Applied after subject compaction so hiding finished books does not
    /// change which branches or single-child chains are collapsed.
    /// Facet counts continue to describe the location including finished books.
    #[serde(default)]
    pub hide_finished: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Dir {
    pub id: DirId,
    pub name: String,
    pub book_count: usize,
    pub downloaded_book_count: usize,
}

/// Where a card's book is, which decides what it offers: a library book is
/// opened, read and downloaded; hosted catalog and feed books are added to the
/// library, while external catalog entries open their source page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BookSource {
    Library {
        content_hash: BookId,
    },
    /// A catalog book the library does not hold; catalog browsing leaves out
    /// the ones it does.
    Catalog {
        content_hash: BookId,
    },
    /// A publication in one of the user's OPDS feeds. It has no file, and so
    /// no content hash, until it is downloaded into the library, where it
    /// is listed as a library book.
    /// `downloadable` when the feed offers it openly, as an EPUB or PDF;
    /// `buyable` when it links a store page that sells it and this system
    /// can open a store window. A book sold only with DRM is not listed.
    Opds {
        source: OpdsSourceId,
        entry: CardKey,
        downloadable: bool,
        buyable: bool,
    },
    /// A catalog entry whose book stays on the source's website.
    /// Its opaque entry key identifies metadata, never a downloaded file.
    /// `downloadable` when the source offers the audiobook as an archive,
    /// which adding the entry downloads into the library.
    ExternalCatalog {
        entry: CardKey,
        downloadable: bool,
    },
}

/// A user's OPDS feed, by the id of its synced entry.
pub type OpdsSourceId = uuid::Uuid;

/// One of the library's OPDS feeds: where it is and what it is called.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpdsSource {
    pub id: OpdsSourceId,
    pub name: String,
    pub url: String,
}

/// A bookstore added on this device, by the id it was added under.
pub type StoreId = uuid::Uuid;

/// A bookstore's website: opened in a window of its own, where what is
/// bought and downloaded goes into the library's root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub id: StoreId,
    pub name: String,
    pub url: String,
}

/// Adding a feed can pause until this device supplies credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AddOpdsFeedResult {
    Added,
    SignInRequired,
}

/// What a feed asks for, sent as HTTP basic auth. Kept on the device that
/// entered it; the feed itself syncs, these do not.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpdsCredentials {
    pub username: String,
    pub password: String,
}

/// Commands carrying credentials are logged; the password must not be.
impl std::fmt::Debug for OpdsCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OpdsCredentials").field("username", &self.username).finish_non_exhaustive()
    }
}

/// What a feed page adds to its listing: the page that continues it, and the
/// feed's own sort and filter choices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPage {
    pub next: Option<BrowseLocation>,
    pub facets: Vec<FeedFacetGroup>,
    /// The feed refused this device: it has no credentials for it, or the
    /// ones it has were refused. The page lists nothing until they are given.
    pub sign_in: bool,
}

/// One of a feed's sort or filter choices ("Sort by", "Language"); each
/// choice is a page of the feed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedFacetGroup {
    pub title: String,
    pub choices: Vec<FeedFacet>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedFacet {
    pub title: String,
    pub location: BrowseLocation,
    pub active: bool,
}

/// Everything a browsing card renders for one book.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookCardRow {
    /// Which book this is, and so what the card offers.
    pub source: BookSource,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    pub author: String,
    pub progress: f32,
    pub downloaded: bool,
    pub download_requested: bool,
    /// Canonical broad format used by filters, facets, and cover chips.
    pub format_category: LibraryFileTypeFilter,
    pub audiobook_duration_ms: Option<u64>,
    /// Milliseconds since the Unix epoch when this book entered the library.
    /// Legacy rows without a timestamp use zero and sort after dated books.
    pub added_at: i64,
    /// Concrete placement represented by a folder-browse result. Cards from
    /// placement-neutral views such as Home and Subjects leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_directory: Option<DirId>,
}

/// What identifies a card in a listing. A book with a file is keyed by its
/// content hash; a book known only by its entry in a feed is keyed by an id
/// derived from that entry. Never a file's identity: code that reaches the
/// book's file uses `content_hash()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CardKey(pub [u8; 32]);

impl From<BookId> for CardKey {
    fn from(id: BookId) -> Self { Self(*id.identity_digest()) }
}

impl From<&BookId> for CardKey {
    fn from(id: &BookId) -> Self { Self::from(*id) }
}

impl std::fmt::Display for CardKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

impl BookCardRow {
    pub fn key(&self) -> CardKey {
        match self.source {
            BookSource::Library { content_hash } | BookSource::Catalog { content_hash, .. } => CardKey::from(content_hash),
            BookSource::Opds { entry, .. } | BookSource::ExternalCatalog { entry, .. } => entry,
        }
    }

    /// The file of a book in the library; none for catalog and feed books,
    /// which are added rather than read or downloaded.
    pub fn library_file(&self) -> Option<BookId> {
        match self.source {
            BookSource::Library { content_hash } => Some(content_hash),
            BookSource::Catalog { .. } | BookSource::Opds { .. } | BookSource::ExternalCatalog { .. } => None,
        }
    }

    /// The book's file, when it has one: a feed book has none until it is
    /// downloaded.
    pub fn content_hash(&self) -> Option<BookId> {
        match self.source {
            BookSource::Library { content_hash } | BookSource::Catalog { content_hash, .. } => Some(content_hash),
            BookSource::Opds { .. } | BookSource::ExternalCatalog { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashedBook {
    pub content_hash: BookId,
    pub title: String,
    pub author: String,
    pub format: String,
    pub deleted_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashedFolder {
    pub id: DirId,
    pub name: String,
    /// The folder it was in; none when that was the library's root.
    pub parent_name: Option<String>,
    pub deleted_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryTrashView {
    pub books: Vec<TrashedBook>,
    pub folders: Vec<TrashedFolder>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFolderDestination {
    pub id: DirId,
    pub parent_id: Option<DirId>,
    pub label: String,
    /// Human-readable hierarchy for flat selectors. This is not a filesystem
    /// location; hierarchical selectors must follow `parent_id`.
    pub path: String,
}

/// One credited author on a book. Only `aut` contributor roles are projected
/// here; editors, translators and narrators remain contributor metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookDetailAuthor {
    pub id: AuthorId,
    pub name: String,
}

/// A narrator credit the detail page states beside the authors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookDetailCredit {
    pub id: AuthorId,
    pub name: String,
    /// The MARC relator code, as stored: "nrt".
    pub role: String,
}

/// Complete initial payload for a book detail screen. Related books are kept
/// in the same response for native callers; browser workers may still resolve
/// the command independently from route rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookDetail {
    pub book: BookCardRow,
    /// The source page for a catalog entry read outside the library.
    #[serde(default)]
    pub external_url: Option<String>,
    /// Navigation entry count for an audiobook, when its contents are known.
    #[serde(default)]
    pub audiobook_chapter_count: Option<usize>,
    /// The publisher's blurb, as stored (HTML). Only the detail page and the
    /// book overlay show it, so browse queries never read it.
    pub description: String,
    /// Where the book sits in the subject tree Browse shows: the path to each
    /// subject that lists the book itself, root first, every crumb a subject
    /// page. Curated taxonomy only — never the file's own subject headings.
    /// Empty for a feed book and for a book with no curated subject.
    #[serde(default)]
    pub subjects: Vec<Vec<BrowsePathSegment>>,
    /// Publication year, when known: the file's own, else the metadata server's.
    #[serde(default)]
    pub year: Option<i32>,
    /// One live placement that can act as the source for copy/move actions.
    /// A detail screen is not itself scoped to a folder.
    pub source_directory_id: Option<String>,
    /// The folder trail to that placement, root first, for a page reached
    /// without browsing to it — from the audiobook dock, a search result, or a
    /// link. The book itself is the trail's last crumb and is not repeated here.
    pub path: Vec<BrowsePathSegment>,
    pub authors: Vec<BookDetailAuthor>,
    /// Narrators, in the order the book credits them.
    pub credits: Vec<BookDetailCredit>,
    /// Where the reader is, named: "Against the Tide?" rather than a percentage
    /// nobody can picture. Absent for a book nobody has opened, and for one
    /// whose navigation is not stored.
    pub current_entry: Option<String>,
    /// The same place, as the entry in `toc` rather than as its wording.
    ///
    /// Both, because they answer different questions: a line of prose about the
    /// reader's position wants the title, and a list marking one of its own rows
    /// wants to know *which row* — and two entries can share a title while a
    /// target identifies one of them. Returning only the title made a contents
    /// list that could never mark anything.
    pub current_target: Option<String>,
    pub toc: Vec<BookTocEntry>,
    #[serde(default)]
    pub chapter_read_status: book_model::ChapterReadStatus,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderSort {
    /// Case-insensitive ascending name, with the original spelling as a stable tie-breaker.
    #[default]
    Name,
    /// Descending recursive book count, then ascending name.
    BookCount,
}

impl Dir {
    pub fn new(id: DirId, name: String, book_count: usize, downloaded_book_count: usize) -> Self {
        Self { id, name, book_count, downloaded_book_count }
    }
    pub fn id(&self) -> &DirId {
        &self.id
    }
    pub fn name(&self) -> &String {
        &self.name
    }
    pub fn book_count(&self) -> usize {
        self.book_count
    }
    pub fn downloaded_book_count(&self) -> usize {
        self.downloaded_book_count
    }
}

impl FolderSort {
    pub fn sort(self, directories: &mut [Dir]) {
        match self {
            Self::Name => directories.sort_by_cached_key(|directory| (directory.name.as_str().to_lowercase(), directory.name.as_str().to_owned())),
            Self::BookCount => directories.sort_by_cached_key(|directory| (Reverse(directory.book_count), directory.name.as_str().to_lowercase(), directory.name.as_str().to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_book_count_sort_uses_name_as_a_deterministic_tie_breaker() {
        let mut directories = vec![Dir::new(uuid::Uuid::new_v4(), "Zelda".to_owned(), 2, 0), Dir::new(uuid::Uuid::new_v4(), "Alice".to_owned(), 7, 0), Dir::new(uuid::Uuid::new_v4(), "Bob".to_owned(), 2, 0)];

        FolderSort::BookCount.sort(&mut directories);
        assert_eq!(directories.iter().map(|directory| (directory.name().as_str(), directory.book_count())).collect::<Vec<_>>(), vec![("Alice", 7), ("Bob", 2), ("Zelda", 2)]);
    }
}
