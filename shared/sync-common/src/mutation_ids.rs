use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MutationId(pub(crate) Uuid);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MutationIdError;
impl fmt::Display for MutationIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("mutation id must be a non-nil UUID")
    }
}
impl std::error::Error for MutationIdError {}
impl MutationId {
    #[cfg(feature = "random-ids")]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
    pub fn parse(value: &str) -> Result<Self, MutationIdError> {
        let value = Uuid::parse_str(value).map_err(|_| MutationIdError)?;
        if value.is_nil() {
            Err(MutationIdError)
        } else {
            Ok(Self(value))
        }
    }
}
impl fmt::Display for MutationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl TryFrom<String> for MutationId {
    type Error = MutationIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<MutationId> for String {
    fn from(value: MutationId) -> Self {
        value.to_string()
    }
}
