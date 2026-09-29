//! Shared decoding and selected-choice retention for browse facets.
use super::{invalid_integer_column, options::file_type_query_value};
use library_model::{LibraryFileTypeFilter, LibraryFormatCount, LibraryLanguageCount};

/// Decode the common tagged row shape returned by folder and subject facets.
pub(crate) fn read_facet_rows(run: impl FnOnce(&mut dyn FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<()>) -> rusqlite::Result<()>) -> Result<(Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>), rusqlite::Error> {
    let mut language_counts = Vec::new();
    let mut format_counts = Vec::new();
    run(&mut |row| {
        let count = row.get::<_, i64>(2)?;
        let book_count = u32::try_from(count).map_err(|_| invalid_integer_column(2, "facet count must fit in u32"))?;
        match row.get::<_, i64>(0)? {
            0 => {
                let format = match row.get::<_, i64>(1)? {
                    1 => LibraryFileTypeFilter::Book,
                    2 => LibraryFileTypeFilter::Pdf,
                    4 => LibraryFileTypeFilter::Audiobook,
                    _ => return Err(invalid_integer_column(1, "unknown browse format category")),
                };
                format_counts.push(LibraryFormatCount { format, book_count });
            }
            1 => language_counts.push(LibraryLanguageCount { language: row.get(1)?, book_count }),
            _ => return Err(invalid_integer_column(0, "unknown facet kind")),
        }
        Ok(())
    })?;
    Ok((language_counts, format_counts))
}

pub(crate) fn retain_selected_facet_choices(language_counts: &mut Vec<LibraryLanguageCount>, format_counts: &mut Vec<LibraryFormatCount>, languages: &str, file_types: i32) {
    for language in languages.trim_matches(',').split(',').filter(|language| !language.is_empty()) {
        if !language_counts.iter().any(|count| count.language == language) {
            language_counts.push(LibraryLanguageCount { language: language.to_owned(), book_count: 0 });
        }
    }
    for (mask, format) in [(1, LibraryFileTypeFilter::Book), (2, LibraryFileTypeFilter::Pdf), (4, LibraryFileTypeFilter::Audiobook)] {
        if file_types & mask != 0 && !format_counts.iter().any(|count| count.format == format) {
            format_counts.push(LibraryFormatCount { format, book_count: 0 });
        }
    }
    language_counts.sort_by(|left, right| left.language.cmp(&right.language));
    format_counts.sort_by_key(|count| file_type_query_value(count.format));
}
