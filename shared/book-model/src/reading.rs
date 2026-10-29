use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EpubCfi(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidEpubCfi;

impl EpubCfi {
    pub fn parse(value: &str) -> Result<Self, InvalidEpubCfi> {
        if !valid_cfi(value) {
            return Err(InvalidEpubCfi);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

fn valid_cfi(value: &str) -> bool {
    if !valid_epub_cfi_shape(value) {
        return false;
    }
    let mut depth = 0_i32;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '^' {
            escaped = true;
            continue;
        }
        match character {
            '[' => depth += 1,
            ']' if depth == 0 => return false,
            ']' => depth -= 1,
            _ => {}
        }
    }
    depth == 0 && !escaped
}

fn valid_epub_cfi_shape(value: &str) -> bool {
    if value.len() > 8 * 1024 || value.trim() != value || value.chars().any(char::is_control) {
        return false;
    }
    value.strip_prefix("epubcfi(").and_then(|value| value.strip_suffix(')')).is_some_and(|inner| !inner.is_empty())
}

/// A canonical reading location shared by all reader formats.
#[derive(Clone, Debug, PartialEq)]
pub enum ReadingPosition {
    Epub(ReadingEpubCfi),
    Pdf(u32),
    PdfAtPoint { page: u32, full_page_position: f32 },
    Audiobook(u64),
    Cbz { page: u32, full_page_position: f32 },
}

/// Protocol-level EPUB position. Unlike an annotation anchor, synchronized
/// reading progress validates only bounded CFI shape and leaves full grammar
/// interpretation to the reader implementation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReadingEpubCfi(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidReadingPosition;

impl ReadingPosition {
    pub fn parse(value: &str) -> Result<Self, InvalidReadingPosition> {
        if valid_epub_cfi(value) {
            return Ok(Self::Epub(ReadingEpubCfi(value.to_owned())));
        }
        if let Some((page, full_page_position)) = parse_pdf_page_position(value) {
            if let Some(full_page_position) = full_page_position {
                return Ok(Self::PdfAtPoint { page, full_page_position });
            }
            return Ok(Self::Pdf(page));
        }
        if let Some(millis) = audiobook_position_millis(value) {
            return Ok(Self::Audiobook(millis));
        }
        if let Some(value) = value.strip_prefix("cbzpage(").and_then(|value| value.strip_suffix(')')) {
            let (page, offset) = value.split_once(',').unwrap_or((value, "0"));
            if page.is_empty() || (page.len() > 1 && page.starts_with('0')) { return Err(InvalidReadingPosition); }
            let page = page.parse().map_err(|_| InvalidReadingPosition)?;
            let full_page_position = offset.parse::<f32>().map_err(|_| InvalidReadingPosition)?;
            if !full_page_position.is_finite() || !(0.0..=1.0).contains(&full_page_position) { return Err(InvalidReadingPosition); }
            return Ok(Self::Cbz { page, full_page_position });
        }
        Err(InvalidReadingPosition)
    }

    pub fn epub_cfi(value: &str) -> Result<Self, InvalidReadingPosition> {
        if valid_epub_cfi(value) { Ok(Self::Epub(ReadingEpubCfi(value.to_owned()))) } else { Err(InvalidReadingPosition) }
    }

    pub const fn pdf_page(page_index: u32) -> Self {
        Self::Pdf(page_index)
    }

    pub const fn pdf_page_at_point(page_index: u32, full_page_position: f32) -> Self {
        Self::PdfAtPoint { page: page_index, full_page_position }
    }

    pub const fn audiobook_millis(millis: u64) -> Self {
        Self::Audiobook(millis)
    }

    /// The beginning of a book in `format`: where a book never opened is
    /// placed when it is marked read. MOBI and FB2 open in the EPUB reader and keep its CFIs, whose
    /// spine is the package's third child, so `/6/2` is its first item.
    pub fn start(format: crate::BookFormat) -> Self {
        match format {
            crate::BookFormat::Epub | crate::BookFormat::Mobi | crate::BookFormat::Fb2 => Self::Epub(ReadingEpubCfi("epubcfi(/6/2)".to_owned())),
            crate::BookFormat::Pdf => Self::Pdf(0),
            crate::BookFormat::M4b | crate::BookFormat::Mp3Folder => Self::Audiobook(0),
            crate::BookFormat::Cbz => Self::Cbz { page: 0, full_page_position: 0.0 },
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            Self::Epub(cfi) => cfi.0.clone(),
            Self::Pdf(page) => pdf_page_reading_position(*page),
            Self::PdfAtPoint { page, full_page_position } => pdf_page_position_reading_position(*page, *full_page_position),
            Self::Audiobook(millis) => audiobook_reading_position(*millis),
            Self::Cbz { page, full_page_position } => format!("cbzpage({page},{full_page_position})"),
        }
    }

    pub fn epub_cfi_value(&self) -> Option<&str> {
        match self {
            Self::Epub(cfi) => Some(&cfi.0),
            _ => None,
        }
    }

    pub fn pdf_page_index(&self) -> Option<u32> {
        match self {
            Self::Pdf(page) => Some(*page),
            Self::PdfAtPoint { page, .. } => Some(*page),
            _ => None,
        }
    }

    pub fn pdf_page_position(&self) -> Option<f32> {
        match self {
            Self::PdfAtPoint { full_page_position, .. } => Some(*full_page_position),
            _ => None,
        }
    }

    pub fn audiobook_position_millis(&self) -> Option<u64> {
        match self {
            Self::Audiobook(millis) => Some(*millis),
            _ => None,
        }
    }
}

impl Eq for ReadingPosition {}

impl std::hash::Hash for ReadingPosition {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Epub(value) => {
                0_u8.hash(state);
                value.hash(state);
            }
            Self::Pdf(page) => {
                1_u8.hash(state);
                page.hash(state);
            }
            Self::PdfAtPoint { page, full_page_position } => {
                2_u8.hash(state);
                page.hash(state);
                full_page_position.to_bits().hash(state);
            }
            Self::Audiobook(millis) => {
                3_u8.hash(state);
                millis.hash(state);
            }
            Self::Cbz { page, full_page_position } => {
                4_u8.hash(state);
                page.hash(state);
                full_page_position.to_bits().hash(state);
            }
        }
    }
}

impl fmt::Display for InvalidReadingPosition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid canonical reading position")
    }
}

impl Error for InvalidReadingPosition {}

impl serde::Serialize for ReadingPosition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ReadingPosition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

pub fn valid_epub_cfi(value: &str) -> bool {
    valid_epub_cfi_shape(value)
}

pub fn valid_reading_position(value: &str) -> bool {
    ReadingPosition::parse(value).is_ok()
}

pub fn pdf_page_from_reading_position(value: &str) -> Option<u32> {
    parse_pdf_page_position(value).map(|(page, _)| page)
}

pub fn pdf_page_position_from_reading_position(value: &str) -> Option<f32> {
    parse_pdf_page_position(value).and_then(|(_, full_page_position)| full_page_position)
}

fn parse_pdf_page_position(value: &str) -> Option<(u32, Option<f32>)> {
    let value = value.strip_prefix("pdfpage(")?.strip_suffix(')')?;
    let (page, position) = value.split_once(',').map_or((value, None), |(page, position)| (page, Some(position)));
    if page.is_empty() || (page.len() > 1 && page.starts_with('0')) {
        return None;
    }
    let page = page.parse::<u32>().ok()?;
    let position = match position {
        Some(position) => Some(parse_full_page_position(position)?),
        None => None,
    };
    Some((page, position))
}

fn parse_full_page_position(value: &str) -> Option<f32> {
    let full_page_position = value.parse::<f32>().ok()?;
    if !full_page_position.is_finite() {
        return None;
    }
    Some(full_page_position.max(0.0))
}

/// How far through the book a position is, on the shared 0–100 progress
/// scale that reading positions and annotations both store. A PDF counts the
/// page being read as reached, so the last page is 100.
pub fn pdf_page_progress(page_index: usize, page_count: usize) -> f32 {
    if page_count == 0 { 0.0 } else { (page_index.saturating_add(1) as f32 / page_count as f32 * 100.0).clamp(0.0, 100.0) }
}

/// The same scale for a reflowed book, whose renderer reports a 0–1 layout
/// fraction.
pub fn layout_fraction_progress(fraction: f32) -> f32 {
    fraction.clamp(0.0, 1.0) * 100.0
}

pub fn pdf_page_reading_position(page_index: u32) -> String {
    format!("pdfpage({page_index})")
}
pub fn pdf_page_position_reading_position(page_index: u32, full_page_position: f32) -> String {
    format!("pdfpage({page_index},{full_page_position})")
}

pub fn audiobook_position_millis(value: &str) -> Option<u64> {
    let millis = value.strip_prefix("audiopos(")?.strip_suffix(')')?;
    if millis.is_empty() || (millis.len() > 1 && millis.starts_with('0')) {
        return None;
    }
    millis.parse().ok()
}

pub fn audiobook_reading_position(millis: u64) -> String {
    format!("audiopos({millis})")
}

impl fmt::Display for EpubCfi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Display for InvalidEpubCfi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("reading position must be a bounded, well-formed EPUB CFI")
    }
}

impl Error for InvalidEpubCfi {}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct ReadProgress(f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidReadProgress;

impl ReadProgress {
    pub const ZERO: Self = Self(0.0);

    pub fn new(value: f32) -> Result<Self, InvalidReadProgress> {
        if value.is_finite() && (0.0..=100.0).contains(&value) { Ok(Self(value)) } else { Err(InvalidReadProgress) }
    }

    pub fn value(self) -> f32 {
        self.0
    }
}

impl serde::Serialize for ReadProgress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f32(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for ReadProgress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <f32 as serde::Deserialize>::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for InvalidReadProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("reading progress must be finite and between 0 and 100")
    }
}

impl Error for InvalidReadProgress {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comic_positions_round_trip_and_reject_invalid_offsets() {
        let position = ReadingPosition::Cbz { page: 12, full_page_position: 0.375 };
        assert_eq!(ReadingPosition::parse(&position.as_str()).unwrap(), position);
        assert_eq!(ReadingPosition::parse("cbzpage(0)").unwrap().as_str(), "cbzpage(0,0)");
        for value in ["cbzpage(-1)", "cbzpage(01)", "cbzpage(1,NaN)", "cbzpage(1,inf)", "cbzpage(1,-0.1)", "cbzpage(1,1.1)", "cbzpage(1,0,0)"] { assert!(ReadingPosition::parse(value).is_err(), "{value}"); }
    }

    #[test]
    fn epub_cfi_rejects_legacy_numbers_and_malformed_delimiters() {
        assert_eq!(EpubCfi::parse("epubcfi(/6/4[id]!/4/2/1:25)").unwrap().as_str(), "epubcfi(/6/4[id]!/4/2/1:25)");
        assert!(EpubCfi::parse("0").is_err());
        assert!(EpubCfi::parse("epubcfi()").is_err());
        assert!(EpubCfi::parse("epubcfi(/6/4[open)").is_err());
        assert!(EpubCfi::parse(" epubcfi(/6/4)").is_err());
    }


}
