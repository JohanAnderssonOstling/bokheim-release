/// Opaque MARC relator code carried by persistence and synchronization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct MarcRelatorCode(pub [u8; 3]);

pub const AUTHOR_MARC_RELATOR_CODE: MarcRelatorCode = MarcRelatorCode(*b"aut");
pub const CONTRIBUTOR_MARC_RELATOR_CODE: MarcRelatorCode = MarcRelatorCode(*b"ctb");
pub const NARRATOR_MARC_RELATOR_CODE: MarcRelatorCode = MarcRelatorCode(*b"nrt");
pub const TRANSLATOR_MARC_RELATOR_CODE: MarcRelatorCode = MarcRelatorCode(*b"trl");

impl MarcRelatorCode {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("ctb")
    }
}

fn parse_three_letter_code(value: &str) -> Option<MarcRelatorCode> {
    let bytes = value.as_bytes();
    if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    Some(MarcRelatorCode([bytes[0].to_ascii_lowercase(), bytes[1].to_ascii_lowercase(), bytes[2].to_ascii_lowercase()]))
}

/// Normalizes input-format contributor roles to their persisted MARC code.
pub fn marc_relator_code_from_metadata_role(value: &str) -> MarcRelatorCode {
    let trimmed = value.trim();
    let token = trimmed.rsplit(['/', '#', ':']).next().unwrap_or(trimmed).trim();
    if let Some(code) = parse_three_letter_code(token) {
        return code;
    }
    let normalized = token.chars().filter(|character| character.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let bytes = match normalized.as_str() {
        "author" | "writer" => return AUTHOR_MARC_RELATOR_CODE,
        "adapter" => b"adp",
        "annotator" => b"ann",
        "artist" => b"art",
        "bookproducer" => b"bkp",
        "blurbwriter" => b"blw",
        "compiler" => b"com",
        "coverartist" | "coverdesigner" => b"cov",
        "editor" => b"edt",
        "illustrator" => b"ill",
        "interviewee" => b"ive",
        "markupeditor" => b"mrk",
        "narrator" => b"nrt",
        "owner" => b"own",
        "performer" => b"prf",
        "proofreader" => b"pfr",
        "sponsor" => b"spn",
        "transcriber" => b"trc",
        "translator" => b"trl",
        "typedesigner" => b"tyd",
        "typographer" => b"tyg",
        "voiceactor" => b"vac",
        _ => return CONTRIBUTOR_MARC_RELATOR_CODE,
    };
    MarcRelatorCode(*bytes)
}
