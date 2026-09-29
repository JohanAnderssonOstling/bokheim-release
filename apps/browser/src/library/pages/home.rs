//! Landing page: progress, recent activity, finished, and recently added.
//!
//! Four shelves rather than a page of its own — the rows, the cards, the cursor
//! and the paging all come from [`ShelfPage`]; this file only decides which
//! books each shelf holds.

use std::collections::HashSet;

use library_backend::LibraryClient;

use crate::library::pages::shelves::{ShelfFuture, ShelfPage, ShelfRow, ShelfSource};

const HOME_BOOK_LIMIT: i32 = 20;

pub(crate) type HomePage = ShelfPage<HomeShelves>;

pub(crate) struct HomeShelves;

impl ShelfSource for HomeShelves {
    const ID: &'static str = "home";
    const LOADING_MESSAGE: &'static str = "Loading library…";

    fn load(library: LibraryClient) -> ShelfFuture {
        Box::pin(async move {
            let mut data = library.home(HOME_BOOK_LIMIT).await?;
            let threshold = library_model::FINISHED_PROGRESS_THRESHOLD;
            // One query returns everything recently read; whether a book belongs
            // under "Recently read" or "Finished" is a question about progress,
            // and it is answered here rather than by a second query.
            let mut in_progress = Vec::new();
            let mut finished = Vec::new();
            for book in data.recently_read.drain(..) {
                if book.progress >= threshold {
                    finished.push(book);
                } else {
                    in_progress.push(book);
                }
            }
            // A book the reader has just opened is recently added as well, and
            // saying so twice on one screen wastes the row that exists to
            // surface what they have not seen yet.
            let recent = data.most_progress.iter().chain(&in_progress).chain(&finished).map(|book| book.content_hash).collect::<HashSet<_>>();
            data.recently_added.retain(|book| !recent.contains(&book.content_hash));

            let rows = [("recently-read", "Recently read", in_progress), ("furthest-along", "Furthest along", data.most_progress), ("finished", "Finished", finished), ("recently-added", "Recently added", data.recently_added)];
            // An empty section is not a section: a library with nothing finished
            // says so by having no Finished shelf, not by an empty row.
            Ok(rows.into_iter().filter(|(_, _, books)| !books.is_empty()).map(|(id, title, books)| ShelfRow { id: id.into(), title: title.into(), books }).collect())
        })
    }
}
