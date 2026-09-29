use crate::BookTocEntry;
use book_model::AuthorId;
use book_model::{AuthorName, AuthorNameError};
use content_address::ContentHash;
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
    content_hash: ContentHash,
}

impl BookLocator {
    pub fn new(library_id: LibraryId, content_hash: ContentHash) -> Self {
        Self { library_id, content_hash }
    }

    pub fn library_id(&self) -> &LibraryId {
        &self.library_id
    }

    pub fn content_hash(&self) -> ContentHash {
        self.content_hash.clone()
    }
}

/// A change relevant to a currently displayed library page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LibraryUpdate {
    Contents,
    Covers(Vec<ContentHash>),
    Download(crate::BookDownloadStatus),
    Scanning(bool),
}

/// Stable identity for a location in Bokheim's subject hierarchy.
/// This is a taxonomy path, not a filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubjectPath(String);

impl SubjectPath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubjectPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
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
            Self::Book => matches!(extension.to_ascii_lowercase().as_str(), "epub" | "mobi" | "azw" | "azw3"),
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
}

impl BrowseChipSort {
    pub const ALL: [Self; 2] = [Self::Alphabetical, Self::BookCount];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "Alphabetical",
            Self::BookCount => "Book count",
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
}

impl BrowseBookSort {
    pub const ALL: [Self; 2] = [Self::Alphabetical, Self::RecentlyRead];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "Alphabetical",
            Self::RecentlyRead => "Recently read",
        }
    }
}

/// One directly browsable child returned by a library hierarchy query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseRow {
    pub id: String,
    pub name: String,
    /// Parent hierarchy for scoped search results; absent for direct children.
    pub path: Option<String>,
    pub book_count: usize,
    pub downloaded_book_count: usize,
}

/// A titled segment of the flat browse child list. Ranges never include headers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseChipGroup {
    pub parent: BrowseRow,
    pub start: usize,
    pub end: usize,
}

/// One directory in the canonical path to a folder browse location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsePathSegment {
    pub id: String,
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

/// Typed input shared by folder and subject browsing implementations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryBrowseQuery {
    pub location: String,
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
    /// Also shows books assigned directly to the selected subject's immediate
    /// children. Grandchildren remain represented by their subject chips.
    #[serde(default)]
    pub include_direct_child_books: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Dir {
    pub id: DirId,
    pub name: String,
    pub book_count: usize,
    pub downloaded_book_count: usize,
}

/// One subject chip beside an author's name.
///
/// A leaf on its own frequently says nothing — "Communities", "20th century" —
/// so the chip carries the parent it was reached through. A label can sit at
/// several routes; `parent` is the one accounting for most of this author's
/// books under that word, and is absent for a top-level subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorSubject {
    pub label: String,
    pub parent: Option<String>,
}

impl AuthorSubject {
    pub fn new(label: String, parent: Option<String>) -> Self {
        let parent = parent.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
        Self { label, parent }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthorSummary {
    pub id: AuthorId,
    pub name: AuthorName,
    /// Every book credited to this identity, which is what the row states.
    /// `books` is capped by the query, so the two disagree for a prolific
    /// author and the count is the one to render.
    pub book_count: usize,
    /// The author's shelf, most recently read first: whole cards, because the
    /// shelf is the same carousel Home draws and its children are book cards.
    /// Capped by the query, so this is a prefix of the author's books rather
    /// than all of them.
    pub books: Vec<BookCardRow>,
    /// What this author writes about: the subjects most of their books are
    /// filed under, most-credited first and capped for display, which the
    /// index draws as chips beside the name.
    pub subjects: Vec<AuthorSubject>,
}

/// Everything a browsing card renders for one book.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookCardRow {
    pub content_hash: ContentHash,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    pub author: String,
    pub description: String,
    pub progress: f32,
    pub downloaded: bool,
    pub download_requested: bool,
    /// Canonical broad format used by filters, facets, and cover chips.
    pub format_category: LibraryFileTypeFilter,
    pub audiobook_duration_ms: Option<u64>,
    pub audiobook_chapter_count: Option<usize>,
    /// Milliseconds since the Unix epoch when this book entered the library.
    /// Legacy rows without a timestamp use zero and sort after dated books.
    pub added_at: i64,
    /// Concrete placement represented by a folder-browse result. Cards from
    /// placement-neutral views such as Home and Subjects leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_directory: Option<DirId>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryHomeView {
    /// Unfinished books ordered by progress, highest first.
    #[serde(default)]
    pub most_progress: Vec<BookCardRow>,
    pub recently_read: Vec<BookCardRow>,
    pub recently_added: Vec<BookCardRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashedBook {
    pub content_hash: ContentHash,
    pub title: String,
    pub format: String,
    pub deleted_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashedFolder {
    pub id: DirId,
    pub name: String,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudiobookChapter {
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// One credited author on a book. Only `aut` contributor roles are projected
/// here; editors, translators and narrators remain contributor metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookDetailAuthor {
    pub id: AuthorId,
    pub name: String,
}

/// A credit the detail page states beside the authors.
///
/// Only the roles a reader chooses a book by: a narrator is why one audiobook
/// is picked over another, and a translator is why one edition is. The rest —
/// typographer, book producer, proofreader — describe how the file was made,
/// and a survey of a real library found `bkp` on half its books naming the
/// conversion tool. They stay in contributor metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookDetailCredit {
    pub id: AuthorId,
    pub name: String,
    /// The MARC relator code, as stored: "nrt", "trl".
    pub role: String,
}

/// Complete initial payload for a book detail screen. Related books are kept
/// in the same response for native callers; browser workers may still resolve
/// the command independently from route rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookDetail {
    pub book: BookCardRow,
    /// One live placement that can act as the source for copy/move actions.
    /// A detail screen is not itself scoped to a folder.
    pub source_directory_id: Option<String>,
    /// The folder trail to that placement, root first, for a page reached
    /// without browsing to it — from the audiobook dock, a search result, or a
    /// link. The book itself is the trail's last crumb and is not repeated here.
    pub path: Vec<BrowsePathSegment>,
    pub authors: Vec<BookDetailAuthor>,
    /// Canonical full paths. Frontends display their leaf and use the complete
    /// value for navigation and accessible names.
    pub subject_paths: Vec<String>,
    /// Narrators and translators, in the order the book credits them.
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
    pub related: Vec<BookCardRow>,
}

/// One library search with title matches kept ahead of distinct author-only
/// matches. A book that matches both is returned only in `title_matches`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibrarySearchResult {
    pub title_matches: Vec<BookCardRow>,
    pub author_matches: Vec<BookCardRow>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorSort {
    /// Case-insensitive ascending name, with the original spelling as a stable tie-breaker.
    #[default]
    Name,
    /// Descending book count, then ascending name.
    BookCount,
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

impl AuthorSummary {
    /// Authors file under their display name.
    pub fn try_new(id: AuthorId, name: String, book_count: usize, books: Vec<BookCardRow>, subjects: Vec<AuthorSubject>) -> Result<Self, AuthorNameError> {
        let name = AuthorName::parse(&name)?;
        Ok(Self { id, name, book_count, books, subjects })
    }
    pub fn id(&self) -> AuthorId {
        self.id
    }
    pub fn name(&self) -> &AuthorName {
        &self.name
    }
    pub fn book_count(&self) -> usize {
        self.book_count
    }
    pub fn books(&self) -> &[BookCardRow] {
        &self.books
    }
    pub fn subjects(&self) -> &[AuthorSubject] {
        &self.subjects
    }
}

impl AuthorSort {
    pub fn sort(self, authors: &mut [AuthorSummary]) {
        match self {
            Self::Name => authors.sort_by_cached_key(|author| (author.name.as_str().to_lowercase(), author.name.as_str().to_owned(), author.id.to_string())),
            Self::BookCount => authors.sort_by_cached_key(|author| (Reverse(author.book_count), author.name.as_str().to_lowercase(), author.name.as_str().to_owned(), author.id.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::agent_id_from_migration_seed;

    #[test]
    fn author_summary_keeps_display_name_and_distinct_identity() {
        let id = agent_id_from_migration_seed(b"Kerstin Ekman");
        let author = AuthorSummary::try_new(id, "Kerstin Ekman".to_owned(), 9, Vec::new(), Vec::new()).unwrap();

        assert_eq!(author.name().as_str(), "Kerstin Ekman", "the display name is what the reader sees");

        let first_id = agent_id_from_migration_seed(b"first Alex Smith");
        let second_id = agent_id_from_migration_seed(b"second Alex Smith");
        let first = AuthorSummary::try_new(first_id, "Alex Smith".to_owned(), 1, Vec::new(), Vec::new()).unwrap();
        let second = AuthorSummary::try_new(second_id, "Alex Smith".to_owned(), 2, Vec::new(), Vec::new()).unwrap();

        assert_eq!(first.name(), second.name());
        assert_ne!(first.id(), second.id());
        assert_eq!(serde_json::to_value(first).unwrap()["id"], first_id.to_string());
        assert_eq!(serde_json::to_value(second).unwrap()["id"], second_id.to_string());
    }

    #[test]
    fn sorting_by_name_orders_by_the_display_name() {
        let author = |display: &str| AuthorSummary::try_new(agent_id_from_migration_seed(display.as_bytes()), display.to_owned(), 1, Vec::new(), Vec::new()).unwrap();
        let mut authors = vec![author("Kerstin Ekman"), author("Susanna Clarke"), author("Peter Englund")];

        AuthorSort::Name.sort(&mut authors);

        assert_eq!(authors.iter().map(|author| author.name().as_str()).collect::<Vec<_>>(), vec!["Kerstin Ekman", "Peter Englund", "Susanna Clarke"]);
    }

    #[test]
    fn author_name_sort_is_case_insensitive_and_deterministic() {
        let mut authors = vec![
            AuthorSummary::try_new(agent_id_from_migration_seed(b"zelda"), "zelda".to_string(), 3, Vec::new(), Vec::new()).unwrap(),
            AuthorSummary::try_new(agent_id_from_migration_seed(b"Alice"), "Alice".to_string(), 1, Vec::new(), Vec::new()).unwrap(),
            AuthorSummary::try_new(agent_id_from_migration_seed(b"alice"), "alice".to_string(), 2, Vec::new(), Vec::new()).unwrap(),
        ];

        AuthorSort::Name.sort(&mut authors);
        assert_eq!(authors.iter().map(|author| author.name().as_str()).collect::<Vec<_>>(), vec!["Alice", "alice", "zelda"]);
    }

    #[test]
    fn author_book_count_sort_uses_name_as_a_deterministic_tie_breaker() {
        let mut authors = vec![
            AuthorSummary::try_new(agent_id_from_migration_seed(b"Zelda"), "Zelda".to_string(), 2, Vec::new(), Vec::new()).unwrap(),
            AuthorSummary::try_new(agent_id_from_migration_seed(b"Alice"), "Alice".to_string(), 7, Vec::new(), Vec::new()).unwrap(),
            AuthorSummary::try_new(agent_id_from_migration_seed(b"Bob"), "Bob".to_string(), 2, Vec::new(), Vec::new()).unwrap(),
        ];

        AuthorSort::BookCount.sort(&mut authors);
        assert_eq!(authors.iter().map(|author| (author.name().as_str(), author.book_count())).collect::<Vec<_>>(), vec![("Alice", 7), ("Bob", 2), ("Zelda", 2)]);
    }

    #[test]
    fn folder_book_count_sort_uses_name_as_a_deterministic_tie_breaker() {
        let mut directories = vec![Dir::new(uuid::Uuid::new_v4(), "Zelda".to_owned(), 2, 0), Dir::new(uuid::Uuid::new_v4(), "Alice".to_owned(), 7, 0), Dir::new(uuid::Uuid::new_v4(), "Bob".to_owned(), 2, 0)];

        FolderSort::BookCount.sort(&mut directories);
        assert_eq!(directories.iter().map(|directory| (directory.name().as_str(), directory.book_count())).collect::<Vec<_>>(), vec![("Alice", 7), ("Bob", 2), ("Zelda", 2)]);
    }
}
