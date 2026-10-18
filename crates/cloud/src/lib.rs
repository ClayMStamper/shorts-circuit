//! The sync protocol between the desktop app and the optional cloud runner.
//!
//! The desktop hands the runner an approved, immutable publication revision together with its
//! rendered media file and the credential its account needs, and reads back status, published
//! URLs and outcome-unknown cases. [`protocol`] has the request and response bodies,
//! [`credential`] the per-platform secrets (redacted in `Debug`), and [`client`] the HTTPS
//! client. Every write is idempotent; media uploads are content-addressed and resumable.

pub mod client;
pub mod credential;
pub mod protocol;

pub use client::{ClientError, ClientResult, CloudClient, parse_runner_url, sha256_file};
pub use credential::{CloudCredential, Secret};
pub use protocol::{
    AccountPut, AccountStatus, ApiError, CredentialCopy, CredentialSource, ErrorCode, HandoverPut,
    MAX_CHUNK_BYTES, MediaRef, PublicationStatus, ReleaseRequest, UploadStart, UploadStatus,
};
