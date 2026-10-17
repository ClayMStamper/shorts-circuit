//! Platform adapter contract and per-provider implementations.
//!
//! [`Adapter`] is the contract from `docs/architecture.md`: capabilities, validate,
//! begin_upload, upload_step, check_processing, publish, reconcile, fetch_metrics. Each adapter
//! keeps its checkpoints and remote identifiers in its own types; [`ErasedAdapter`] wraps any
//! adapter for the scheduler, which stores those values as opaque JSON.
//!
//! [`tiktok`] and [`broker`] are the source side: TikTok new-post detection through the
//! Display API, with tokens from the OAuth broker. No publishing adapters exist yet (Increment 3). Accounts without one use
//! [`manual::ManualHandoff`], which writes a publishing package to a folder.

pub mod broker;
pub mod contract;
#[doc(hidden)]
pub mod fake;
pub mod manual;
pub mod registry;
pub mod tiktok;

pub use contract::{
    Adapter, AdapterError, ErasedAdapter, Metric, Processing, PublishedPost, Reconciliation,
    Submission, UploadProgress,
};
pub use manual::{ExportedPackage, ManualHandoff, PackageRequest};
pub use registry::AdapterRegistry;
