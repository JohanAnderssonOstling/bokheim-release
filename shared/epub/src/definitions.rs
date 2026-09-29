#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentifierScheme {
    Unspecified,
    ISBN,
    ASIN,
    DOI,
    ISSN,
    OCLC,
    LCCN,
    UUID,
    CALIBRE,
    GOOG,
    GOODREADS,
    KOBO,
    Other(String),
}

/// Role evidence extracted from a book format.
///
/// This layer deliberately does not interpret roles. The application import
/// boundary converts this evidence once into a canonical MARC relator code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatorRole(String);

impl CreatorRole {
    pub fn new(value: impl AsRef<str>) -> Self {
        let value = value.as_ref().trim();
        Self(if value.is_empty() { "ctb".to_owned() } else { value.to_owned() })
    }

    pub fn author() -> Self {
        Self("aut".to_owned())
    }

    pub fn contributor() -> Self {
        Self("ctb".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn parse_identifier_scheme(scheme: &str) -> IdentifierScheme {
    let scheme = scheme.trim();
    match scheme.to_uppercase().as_str() {
        "" => IdentifierScheme::Unspecified,
        "ISBN" => IdentifierScheme::ISBN,
        "ASIN" => IdentifierScheme::ASIN,
        "MOBI-ASIN" => IdentifierScheme::ASIN,
        "AMAZON" => IdentifierScheme::ASIN,
        "OASIN" => IdentifierScheme::ASIN,
        "DOI" => IdentifierScheme::DOI,
        "ISSN" => IdentifierScheme::ISSN,
        "OCLC" => IdentifierScheme::OCLC,
        "LCCN" => IdentifierScheme::LCCN,
        "UUID" => IdentifierScheme::UUID,
        "CALIBRE" => IdentifierScheme::CALIBRE,
        "GOODREADS" => IdentifierScheme::GOODREADS,
        "GOOG" | "GOOGLE" => IdentifierScheme::GOOG,
        "KOBO" => IdentifierScheme::KOBO,
        other => {
            if other.contains("ISBN") {
                IdentifierScheme::ISBN
            } else {
                IdentifierScheme::Other(scheme.to_owned())
            }
        }
    }
}

pub fn parse_creator_role(role: &str) -> CreatorRole {
    CreatorRole::new(role)
}
