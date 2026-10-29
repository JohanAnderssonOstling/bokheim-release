//! Recognize CJK books using structured BCP 47 language metadata.

use html::text::LanguageTag;

use crate::book::NativeBook;
use ui_components::CJK_FONT_FAMILY;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CjkFont {
    Japanese,
    Korean,
    SimplifiedChinese,
    TraditionalChinese,
    HongKong,
}

impl CjkFont {
    /// Declared language controls the font setting; glyph fallback is independent.
    pub(crate) fn for_book(book: &NativeBook) -> Option<Self> {
        Self::for_languages(&book.languages())
    }

    pub(crate) fn for_languages(languages: &[String]) -> Option<Self> {
        languages.iter().filter_map(|language| LanguageTag::parse(language.trim()).ok()).find_map(|tag| Self::for_tag(&tag))
    }

    fn for_tag(tag: &LanguageTag) -> Option<Self> {
        let script = tag.script();
        // An explicit script takes precedence, including romanized CJK text.
        match script {
            Some("Jpan" | "Hira" | "Kana") => return Some(Self::Japanese),
            Some("Kore" | "Hang" | "Jamo") => return Some(Self::Korean),
            Some("Hans") => return Some(Self::SimplifiedChinese),
            Some("Hant" | "Hani" | "Hanb" | "Bopo") | None => {}
            Some(_) => return None,
        }
        match tag.primary_language() {
            "ja" => Some(Self::Japanese),
            "ko" => Some(Self::Korean),
            "zh" | "cmn" | "yue" | "nan" | "hak" | "wuu" | "gan" | "hsn" | "lzh" => Some(match (script, tag.region(), tag.primary_language(), tag.extended_language()) {
                (_, Some("HK" | "MO"), _, _) => Self::HongKong,
                (Some("Hant" | "Bopo"), _, _, _) | (_, Some("TW"), _, _) => Self::TraditionalChinese,
                (_, None, "yue", _) | (_, None, _, Some("yue")) => Self::HongKong,
                _ => Self::SimplifiedChinese,
            }),
            _ => match script {
                Some("Hant" | "Bopo") => Some(Self::TraditionalChinese),
                Some("Hani" | "Hanb") => Some(Self::SimplifiedChinese),
                _ => None,
            },
        }
    }

    pub(crate) fn family(self) -> &'static str {
        CJK_FONT_FAMILY
    }

    pub(crate) fn sample(self) -> &'static str {
        match self {
            Self::Japanese => "あ漢",
            Self::Korean => "한글",
            Self::SimplifiedChinese => "汉字",
            Self::TraditionalChinese | Self::HongKong => "漢字",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_content::BookFormat;
    use std::io::{Cursor, Write};

    fn book(language: Option<&str>, chapter: &str) -> NativeBook {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let language = language.map(|tag| format!("<dc:language>{tag}</dc:language>")).unwrap_or_default();
        let package = format!("<package xmlns='http://www.idpf.org/2007/opf'><metadata xmlns:dc='http://purl.org/dc/elements/1.1/'>{language}</metadata><manifest><item id='chapter' href='chapter.xhtml' media-type='application/xhtml+xml'/></manifest><spine><itemref idref='chapter'/></spine></package>");
        for (name, bytes) in [
            ("META-INF/container.xml", "<container><rootfiles><rootfile full-path='content.opf'/></rootfiles></container>"),
            ("content.opf", package.as_str()),
            ("chapter.xhtml", chapter),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes.as_bytes()).unwrap();
        }
        NativeBook::from_reader(BookFormat::Epub, zip.finish().unwrap()).unwrap()
    }

    #[test]
    fn cjk_controls_require_declared_book_language() {
        let chapter = "<html xml:lang='ja'><body>日本語の本を読みます。</body></html>";
        for language in [None, Some("und"), Some("invalid_tag"), Some("en"), Some("ar"), Some("he")] {
            assert_eq!(CjkFont::for_book(&book(language, chapter)), None, "{language:?}");
        }
    }

    #[test]
    fn metadata_selects_the_profile_independently_of_chapter_text() {
        for chapter in ["<body>A book in English.</body>", "<body><img src='cover.png'/></body>"] {
            assert_eq!(CjkFont::for_book(&book(Some("ja"), chapter)), Some(CjkFont::Japanese));
            assert_eq!(CjkFont::for_book(&book(Some("zh-Hant"), chapter)), Some(CjkFont::TraditionalChinese));
        }
    }

    #[test]
    fn cjk_detection_follows_bcp47_script_and_region() {
        for (tag, expected) in [
            ("JA-jp", CjkFont::Japanese),
            ("ko-KR", CjkFont::Korean),
            ("zh", CjkFont::SimplifiedChinese),
            ("zh-Hans-TW", CjkFont::SimplifiedChinese),
            ("zh-Hant-CN", CjkFont::TraditionalChinese),
            ("zh-TW", CjkFont::TraditionalChinese),
            ("zh-Hant-HK", CjkFont::HongKong),
            ("zh-MO", CjkFont::HongKong),
            ("yue", CjkFont::HongKong),
            ("zh-yue", CjkFont::HongKong),
            ("und-Jpan", CjkFont::Japanese),
            ("und-Hang", CjkFont::Korean),
        ] {
            assert_eq!(CjkFont::for_languages(&[tag.into()]), Some(expected), "{tag}");
        }
    }

    #[test]
    fn latin_and_rtl_metadata_do_not_enable_cjk_controls() {
        for tag in ["en", "sv-SE", "ar", "he", "ja-Latn", "zh-Latn", "not_a_tag"] {
            assert_eq!(CjkFont::for_languages(&[tag.into()]), None, "{tag}");
        }
        assert_eq!(CjkFont::for_languages(&[]), None);
        assert_eq!(CjkFont::for_languages(&["invalid_tag".into(), "en".into(), "ja".into()]), Some(CjkFont::Japanese));
    }
}
