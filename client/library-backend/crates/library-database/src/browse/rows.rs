//! Decode database rows without masking malformed values.
use library_model::{BookCardRow, LibraryFileTypeFilter};
use sync_common::ContentHash;

pub(crate) fn invalid_text_column(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())))
}

pub(crate) fn invalid_integer_column(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Integer, Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())))
}

pub(crate) fn parse_uuid_column(value: String, column: usize) -> rusqlite::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(&value).map_err(|error| invalid_text_column(column, error.to_string()))
}

/// Every card query supplies the same named columns, including nullable source_directory.
pub(crate) fn read_book_card_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BookCardRow> {
    let raw_content_hash: String = row.get("content_hash")?;
    let title: Option<String> = row.get("title")?;
    let duration_column = row.as_ref().column_index("audiobook_duration_ms")?;
    let chapter_column = row.as_ref().column_index("audiobook_chapter_count")?;
    let source_column = row.as_ref().column_index("source_directory")?;
    let duration_ms: Option<i64> = row.get(duration_column)?;
    let chapter_count: Option<i64> = row.get(chapter_column)?;
    let source: Option<String> = row.get(source_column)?;
    Ok(BookCardRow {
        content_hash: ContentHash::new(&raw_content_hash),
        title: title.unwrap_or_else(|| format!("Untitled ({raw_content_hash})")),
        subtitle: row.get("subtitle")?,
        author: row.get::<_, Option<String>>("author")?.unwrap_or_default(),
        description: row.get::<_, Option<String>>("description")?.unwrap_or_default(),
        progress: row.get::<_, Option<f32>>("progress")?.unwrap_or_default(),
        downloaded: row.get("downloaded")?,
        download_requested: row.get("download_requested")?,
        format_category: match row.get::<_, i64>("format_category")? {
            1 => LibraryFileTypeFilter::Book,
            2 => LibraryFileTypeFilter::Pdf,
            4 => LibraryFileTypeFilter::Audiobook,
            value => return Err(invalid_integer_column(row.as_ref().column_index("format_category")?, format!("invalid book format category {value}"))),
        },
        audiobook_duration_ms: duration_ms.map(u64::try_from).transpose().map_err(|e| invalid_integer_column(duration_column, e.to_string()))?,
        audiobook_chapter_count: chapter_count.map(usize::try_from).transpose().map_err(|e| invalid_integer_column(chapter_column, e.to_string()))?,
        added_at: row.get::<_, Option<i64>>("added_at")?.unwrap_or_default(),
        source_directory: source.map(|value| parse_uuid_column(value, source_column)).transpose()?,
    })
}
