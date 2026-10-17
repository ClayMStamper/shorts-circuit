//! Platform adapter contract and per-provider implementations.
//!
//! [`Adapter`] is the contract from `docs/architecture.md`: capabilities, validate,
//! begin_upload, upload_step, check_processing, publish, reconcile, fetch_metrics. Each adapter
//! keeps its checkpoints and remote identifiers in its own types; [`ErasedAdapter`] wraps any
//! adapter for the scheduler, which stores those values as opaque JSON.
//!
//! [`youtube`] publishes YouTube Shorts through the Data API with resumable uploads, using
//! tokens from Google's installed-app OAuth flow ([`google`]). [`instagram`] publishes Reels
//! through the Instagram API with Instagram Login, uploading the file to a media container;
//! [`linkedin`] posts videos as a Company Page through the Videos and Posts APIs. [`tiktok`] and [`broker`] are
//! the source side: TikTok new-post detection through the Display API, with tokens from the
//! OAuth broker. Accounts without an adapter (or without API access) use
//! [`manual::ManualHandoff`], which writes a publishing package to a folder.

pub mod broker;
pub mod contract;
#[doc(hidden)]
pub mod fake;
pub mod google;
pub mod instagram;
pub mod linkedin;
pub mod manual;
pub mod registry;
pub mod tiktok;
pub mod youtube;

pub use contract::{
    Adapter, AdapterError, ErasedAdapter, Metric, Processing, PublishedPost, Reconciliation,
    Submission, UploadProgress,
};
pub use manual::{ExportedPackage, ManualHandoff, PackageRequest};
pub use registry::AdapterRegistry;
