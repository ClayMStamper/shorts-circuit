//! Request and response bodies of the sync API. Paths are under `/v1`; every request except
//! `GET /healthz` carries `Authorization: Bearer <installation token>`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shorts_domain::{
    AccountId, ConnectedAccount, HandoverId, MediaProperties, OwnerReport, PublicationRecord,
};

use crate::credential::CloudCredential;

/// Largest chunk the runner accepts in one `PUT /v1/uploads/{sha256}`.
pub const MAX_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// `POST /v1/uploads`: start (or look up) the upload of a file identified by its SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadStart {
    /// Lowercase hex.
    pub sha256: String,
    pub size_bytes: u64,
}

/// Where an upload stands. `PUT /v1/uploads/{sha256}?offset=N` with the next bytes moves it on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadStatus {
    pub sha256: String,
    pub size_bytes: u64,
    pub received_bytes: u64,
    /// Every byte arrived and the file matched its SHA-256.
    pub complete: bool,
}

/// `PUT /v1/accounts/{id}`: the destination account and the credential the runner needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountPut {
    pub account: ConnectedAccount,
    pub credential: CloudCredential,
    /// The credential revision this desktop last saw from the runner (`None` the first time).
    /// If the runner has a newer one (it rotated a refresh token), the put is refused with
    /// `credential_conflict` and the desktop pulls that one first.
    pub base_revision: Option<u64>,
}

/// Who wrote a stored credential last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Desktop,
    /// The runner refreshed or renewed it, so the desktop's copy may be out of date.
    Runner,
}

/// An account as the runner holds it (`GET /v1/accounts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub account_id: AccountId,
    pub credential_revision: u64,
    pub updated_by: CredentialSource,
    pub updated_at: DateTime<Utc>,
}

/// `GET /v1/accounts/{id}/credential`: the runner's current credential, after it rotated one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialCopy {
    pub revision: u64,
    pub credential: CloudCredential,
}

/// The rendered file a hand-over publishes, uploaded first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    pub sha256: String,
    pub size_bytes: u64,
    pub properties: MediaProperties,
}

/// `PUT /v1/publications/{id}`: hand an approved publication revision to the runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoverPut {
    pub handover: HandoverId,
    /// The desktop's record right after `HandToCloud`: cloud-owned, scheduled, approved.
    pub publication: PublicationRecord,
    pub media: MediaRef,
}

/// `POST /v1/publications/{id}/release`: give the publication back to the desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRequest {
    pub handover: HandoverId,
}

/// A publication as the runner sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationStatus {
    pub report: OwnerReport,
    /// Upload progress, when uploading.
    #[serde(default)]
    pub progress_fraction: Option<f64>,
    /// What the platform says it is doing, when it says.
    #[serde(default)]
    pub progress_detail: Option<String>,
}

/// Every error answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    /// With `offset_mismatch`: where the upload actually is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_bytes: Option<u64>,
    /// With `credential_conflict`: the runner's credential revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Missing, unknown or revoked installation token (401).
    Unauthorized,
    NotFound,
    /// The request contradicts what the runner holds (409).
    Conflict,
    /// The hand-over was already taken back (410).
    Gone,
    /// The cloud attempt is running or will resume an upload the platform has (409).
    InFlight,
    /// The chunk's offset isn't where the upload is (409); see `received_bytes`.
    OffsetMismatch,
    /// The runner has a newer credential (409); see `revision`.
    CredentialConflict,
    /// The hand-over's media hasn't been uploaded completely (409).
    MediaIncomplete,
    /// The request is malformed or refused by the domain (422).
    Invalid,
    /// The upload's bytes don't match its SHA-256 (422); start it again.
    ChecksumMismatch,
    Internal,
}
