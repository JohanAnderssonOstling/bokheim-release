use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthority {
    Wikidata,
    OpenLibrary,
    Viaf,
    Isni,
    Orcid,
    LibraryOfCongress,
}

impl AgentAuthority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wikidata => "wikidata",
            Self::OpenLibrary => "open_library",
            Self::Viaf => "viaf",
            Self::Isni => "isni",
            Self::Orcid => "orcid",
            Self::LibraryOfCongress => "library_of_congress",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AgentIdentityError> {
        match value {
            "wikidata" => Ok(Self::Wikidata),
            "open_library" => Ok(Self::OpenLibrary),
            "viaf" => Ok(Self::Viaf),
            "isni" => Ok(Self::Isni),
            "orcid" => Ok(Self::Orcid),
            "library_of_congress" => Ok(Self::LibraryOfCongress),
            _ => Err(AgentIdentityError("unsupported agent authority")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentIdentityError(&'static str);

impl std::fmt::Display for AgentIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for AgentIdentityError {}

macro_rules! string_identifier {
    ($name:ident, $canonicalizer:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, AgentIdentityError> {
                $canonicalizer(value).map(Self)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

string_identifier!(WikidataItemId, canonical_wikidata_id);
string_identifier!(OpenLibraryAuthorId, canonical_open_library_author_id);
string_identifier!(ViafId, canonical_viaf_id);
string_identifier!(Isni, canonical_isni);
string_identifier!(Orcid, canonical_orcid);
string_identifier!(LibraryOfCongressAuthorityId, canonical_library_of_congress_id);

/// A globally or provider-defined agent identity whose authority and value
/// cannot be combined incorrectly in memory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ExternalAgentId {
    Wikidata(WikidataItemId),
    OpenLibrary(OpenLibraryAuthorId),
    Viaf(ViafId),
    Isni(Isni),
    Orcid(Orcid),
    LibraryOfCongress(LibraryOfCongressAuthorityId),
}

impl ExternalAgentId {
    pub fn parse(authority: AgentAuthority, value: &str) -> Result<Self, AgentIdentityError> {
        match authority {
            AgentAuthority::Wikidata => WikidataItemId::parse(value).map(Self::Wikidata),
            AgentAuthority::OpenLibrary => OpenLibraryAuthorId::parse(value).map(Self::OpenLibrary),
            AgentAuthority::Viaf => ViafId::parse(value).map(Self::Viaf),
            AgentAuthority::Isni => Isni::parse(value).map(Self::Isni),
            AgentAuthority::Orcid => Orcid::parse(value).map(Self::Orcid),
            AgentAuthority::LibraryOfCongress => LibraryOfCongressAuthorityId::parse(value).map(Self::LibraryOfCongress),
        }
    }

    pub const fn authority(&self) -> AgentAuthority {
        match self {
            Self::Wikidata(_) => AgentAuthority::Wikidata,
            Self::OpenLibrary(_) => AgentAuthority::OpenLibrary,
            Self::Viaf(_) => AgentAuthority::Viaf,
            Self::Isni(_) => AgentAuthority::Isni,
            Self::Orcid(_) => AgentAuthority::Orcid,
            Self::LibraryOfCongress(_) => AgentAuthority::LibraryOfCongress,
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Wikidata(value) => value.as_str(),
            Self::OpenLibrary(value) => value.as_str(),
            Self::Viaf(value) => value.as_str(),
            Self::Isni(value) => value.as_str(),
            Self::Orcid(value) => value.as_str(),
            Self::LibraryOfCongress(value) => value.as_str(),
        }
    }
}

impl Serialize for ExternalAgentId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct EncodedExternalAgentId<'a> {
            authority: AgentAuthority,
            value: &'a str,
        }

        EncodedExternalAgentId { authority: self.authority(), value: self.value() }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExternalAgentId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct EncodedExternalAgentId {
            authority: AgentAuthority,
            value: String,
        }

        let encoded = EncodedExternalAgentId::deserialize(deserializer)?;
        Self::parse(encoded.authority, &encoded.value).map_err(serde::de::Error::custom)
    }
}

macro_rules! external_agent_from {
    ($identifier:ty, $variant:ident) => {
        impl From<$identifier> for ExternalAgentId {
            fn from(value: $identifier) -> Self {
                Self::$variant(value)
            }
        }
    };
}

external_agent_from!(WikidataItemId, Wikidata);
external_agent_from!(OpenLibraryAuthorId, OpenLibrary);
external_agent_from!(ViafId, Viaf);
external_agent_from!(Isni, Isni);
external_agent_from!(Orcid, Orcid);
external_agent_from!(LibraryOfCongressAuthorityId, LibraryOfCongress);

pub type AuthorAuthority = AgentAuthority;
pub type ExternalAuthorId = ExternalAgentId;
pub type AuthorIdentityError = AgentIdentityError;

fn bounded_identifier(value: &str) -> Result<&str, AgentIdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(AgentIdentityError("external agent id must be non-empty, bounded text"))
    } else {
        Ok(value)
    }
}

fn canonical_wikidata_id(value: &str) -> Result<String, AgentIdentityError> {
    let value = bounded_identifier(value)?.to_ascii_uppercase();
    let digits = value.strip_prefix('Q').and_then(canonical_positive_decimal);
    digits.map(|digits| format!("Q{digits}")).ok_or(AgentIdentityError("invalid Wikidata item id"))
}

fn canonical_open_library_author_id(value: &str) -> Result<String, AgentIdentityError> {
    let value = bounded_identifier(value)?;
    let value = value.strip_prefix("/authors/").unwrap_or(value).to_ascii_uppercase();
    let digits = value.strip_prefix("OL").and_then(|value| value.strip_suffix('A')).and_then(canonical_positive_decimal);
    digits.map(|digits| format!("OL{digits}A")).ok_or(AgentIdentityError("invalid Open Library author id"))
}

fn canonical_viaf_id(value: &str) -> Result<String, AgentIdentityError> {
    canonical_positive_decimal(bounded_identifier(value)?).map(str::to_owned).ok_or(AgentIdentityError("invalid VIAF id"))
}

fn canonical_isni(value: &str) -> Result<String, AgentIdentityError> {
    let value = bounded_identifier(value)?;
    let canonical = compact_iso_identifier(value);
    valid_iso_7064_mod_11_2(&canonical).then_some(canonical).ok_or(AgentIdentityError("invalid ISNI"))
}

fn canonical_orcid(value: &str) -> Result<String, AgentIdentityError> {
    let value = bounded_identifier(value)?;
    let canonical = compact_iso_identifier(value);
    valid_iso_7064_mod_11_2(&canonical).then_some(canonical).ok_or(AgentIdentityError("invalid ORCID"))
}

fn canonical_library_of_congress_id(value: &str) -> Result<String, AgentIdentityError> {
    let value = bounded_identifier(value)?;
    let valid = value.len() >= 2 && value.is_ascii() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
    valid.then(|| value.to_ascii_lowercase()).ok_or(AgentIdentityError("invalid Library of Congress authority id"))
}

fn canonical_positive_decimal(value: &str) -> Option<&str> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let canonical = value.trim_start_matches('0');
    (!canonical.is_empty()).then_some(canonical)
}

fn compact_iso_identifier(value: &str) -> String {
    value.chars().filter(|character| !matches!(character, ' ' | '-')).map(|character| character.to_ascii_uppercase()).collect()
}

fn valid_iso_7064_mod_11_2(value: &str) -> bool {
    if value.len() != 16 {
        return false;
    }
    let (body, check) = value.split_at(15);
    if !body.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut total = 0_u32;
    for digit in body.bytes().map(|byte| u32::from(byte - b'0')) {
        total = ((total + digit) * 2) % 11;
    }
    let result = (12 - (total % 11)) % 11;
    let expected = if result == 10 { b'X' } else { b'0' + result as u8 };
    check.as_bytes() == [expected]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_specific_types_canonicalize_and_validate_their_own_domains() {
        assert_eq!(WikidataItemId::parse("q000892").unwrap().as_str(), "Q892");
        assert_eq!(OpenLibraryAuthorId::parse("/authors/ol023919a").unwrap().as_str(), "OL23919A");
        assert_eq!(ViafId::parse("0012345678").unwrap().as_str(), "12345678");
        assert_eq!(Orcid::parse("0000-0002-1825-0097").unwrap().as_str(), "0000000218250097");
        assert_eq!(Isni::parse("0000 0001 2140 0562").unwrap().as_str(), "0000000121400562");
        assert_eq!(LibraryOfCongressAuthorityId::parse("N79-021164").unwrap().as_str(), "n79-021164");
        assert!(WikidataItemId::parse("Tolkien").is_err());
        assert!(Orcid::parse("0000-0002-1825-0098").is_err());
        assert!(Isni::parse("0000 0001 2140 0563").is_err());
    }

    #[test]
    fn external_agent_id_is_a_closed_sum_of_typed_identifiers() {
        let isni = Isni::parse("0000 0001 2140 0562").unwrap();
        let external: ExternalAuthorId = isni.clone().into();
        assert_eq!(external, ExternalAgentId::Isni(isni));
        assert_eq!(external.authority(), AuthorAuthority::Isni);
        assert_eq!(external.value(), "0000000121400562");

        let parsed = ExternalAuthorId::parse(AuthorAuthority::OpenLibrary, "/authors/ol23919a").unwrap();
        assert!(matches!(parsed, ExternalAgentId::OpenLibrary(ref value) if value.as_str() == "OL23919A"));

        let encoded = wire::encode(&external).unwrap();
        assert_eq!(wire::decode::<ExternalAgentId>(&encoded, 1024).unwrap(), external);
    }
}
