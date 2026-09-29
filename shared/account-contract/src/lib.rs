//! Portable account HTTP contracts shared by clients and account transports.

use serde::{Deserialize, Serialize};

pub const PASSWORD_MIN_CHARACTERS: usize = 12;
pub const PASSWORD_MAX_BYTES: usize = 1024;
pub const EMAIL_MAX_BYTES: usize = 254;
pub const EMAIL_VERIFICATION_PIN_DIGITS: usize = 6;
pub const ONE_TIME_TOKEN_BYTES: usize = 43;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicRegistrationRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailRequest {
    pub email: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailVerificationRequest {
    pub email: String,
    pub pin: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthResponse {
    /// Short-lived bearer credential used for ordinary API requests.
    pub token: String,
    /// Absolute Unix timestamp after which the access token is invalid.
    pub expires_at: i64,
    /// Long-lived credential accepted only by the refresh endpoint.
    pub refresh_token: String,
    /// Absolute Unix timestamp after which the refresh token is invalid.
    pub refresh_expires_at: i64,
    pub user_id: String,
    pub email: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeResponse {
    pub user_id: String,
    pub email: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageUsageResponse {
    /// Bytes held by completed, user-owned book and thumbnail assets.
    pub used_bytes: u64,
    /// Bytes temporarily reserved for uploads that have not completed yet.
    pub reserved_bytes: u64,
    pub quota_bytes: u64,
}

/// One library's share of [`StorageUsageResponse::used_bytes`].
///
/// A separate response rather than a field on `StorageUsageResponse`: the
/// storage endpoint is already deployed, and adding a field to its encoding
/// would break clients and servers that disagree about the version. Clients
/// treat a missing route the same way they already treat a server with no quota
/// endpoint at all.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LibraryStorageUsage {
    pub library_id: String,
    /// Bytes charged to the account and attributed to this library. The
    /// attributions across libraries partition the account's used bytes, so a
    /// book held by two libraries is counted under one of them only. A thumbnail
    /// is attributed to the first library that uploads it.
    pub used_bytes: u64,
}

/// Everything an account settings screen needs in one server response.
/// Device-local storage remains a client concern.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountSnapshotResponse {
    pub libraries: Vec<sync_common::api::libraries::LibrarySummary>,
    pub deleted_library_ids: Vec<sync_common::LibraryId>,
    pub storage: StorageUsageResponse,
    pub library_storage: Vec<LibraryStorageUsage>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub current: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRevocationResponse {
    pub revoked: bool,
    pub signed_out: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OtherSessionsRevocationResponse {
    pub revoked: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_and_refresh_credentials_have_distinct_wire_fields() {
        let response = AuthResponse { token: "access".to_owned(), expires_at: 100, refresh_token: "refresh".to_owned(), refresh_expires_at: 200, user_id: "user-1".to_owned(), email: "reader@example.com".to_owned() };
        let encoded = wire::encode(&response).unwrap();
        let value: AuthResponse = wire::decode(&encoded, 1024).unwrap();
        assert_eq!(value.token, "access");
        assert_eq!(value.refresh_token, "refresh");
    }

    #[test]
    fn session_summaries_expose_identifiers_and_times_but_no_credentials() {
        let summary = SessionSummary { session_id: "session-1".to_owned(), created_at: 100, expires_at: 200, current: true };
        let encoded = wire::encode(&summary).unwrap();
        let value: SessionSummary = wire::decode(&encoded, 1024).unwrap();
        assert_eq!(value, summary);
    }

    #[test]
    fn storage_usage_round_trips_without_losing_large_byte_counts() {
        let usage = StorageUsageResponse { used_bytes: 1_000_000_000_000, reserved_bytes: 42, quota_bytes: 2_000_000_000_000 };
        let encoded = wire::encode(&usage).unwrap();
        assert_eq!(wire::decode::<StorageUsageResponse>(&encoded, 1024).unwrap(), usage);
    }

    #[test]
    fn account_snapshot_round_trips_all_remote_settings_state() {
        let library_id = sync_common::LibraryId::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let snapshot = AccountSnapshotResponse {
            libraries: vec![sync_common::api::libraries::LibrarySummary { library_id, library_name: "Classics".to_owned() }],
            deleted_library_ids: Vec::new(),
            storage: StorageUsageResponse { used_bytes: 50, reserved_bytes: 5, quota_bytes: 100 },
            library_storage: vec![LibraryStorageUsage { library_id: library_id.to_string(), used_bytes: 50 }],
        };
        let encoded = wire::encode(&snapshot).unwrap();
        assert_eq!(wire::decode::<AccountSnapshotResponse>(&encoded, 4096).unwrap(), snapshot);
    }
}
