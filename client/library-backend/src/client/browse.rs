use super::LibraryClient;
use crate::library::commands::library_requests;
use crate::LibraryBrowseData;

impl LibraryClient {
    pub async fn book_count(&self) -> Result<usize, String> {
        self.request(library_requests::BookCount).await
    }
    pub async fn home(&self, limit: i32) -> Result<library_model::LibraryHomeView, String> {
        self.request(library_requests::Home { limit }).await
    }
    pub async fn folder_contents(&self, query: library_model::LibraryBrowseQuery) -> Result<LibraryBrowseData, String> {
        self.request(library_requests::FolderContents { query }).await
    }
    pub async fn subject_contents(&self, query: library_model::LibraryBrowseQuery) -> Result<LibraryBrowseData, String> {
        self.request(library_requests::SubjectContents { query }).await
    }
    pub async fn library_search(&self, query: String) -> Result<library_model::LibrarySearchResult, String> {
        self.request(library_requests::LibrarySearch { query }).await
    }
    pub async fn book_detail(&self, content_hash: crate::ContentHash) -> Result<library_model::BookDetail, String> {
        self.request(library_requests::BookDetail { content_hash }).await
    }
    pub async fn authors(&self, sort: library_model::AuthorSort) -> Result<Vec<library_model::AuthorSummary>, String> {
        self.request(library_requests::Authors { sort }).await
    }
}
