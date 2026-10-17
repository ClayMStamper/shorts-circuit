//! Durable job execution: leases, heartbeats, retries with jitter, checkpoints, recovery.
//!
//! The [`Worker`] claims jobs from the store under a lease, renews the lease with heartbeats
//! while a [`JobHandler`] runs, and turns the handler's result into a [`Disposition`]: success,
//! a jittered retry, a rate-limit delay, a pause for the user, a failure, or expiry under the
//! [`OverduePolicy`](shorts_domain::OverduePolicy). Handlers are traits; the scheduler never
//! knows about concrete adapters or media tools.
//!
//! Posting slots ([`slots`]) turn the user's local posting times into UTC instants, handling
//! daylight-saving gaps and overlaps explicitly.

pub mod clock;
pub mod handler;
pub mod retry;
pub mod slots;
pub mod worker;

pub use clock::{Clock, ManualClock, SystemClock};
pub use handler::{Disposition, JobContext, JobFailure, JobHandler};
pub use retry::RetryPolicy;
pub use slots::{PostingPlan, PostingSlot, resolve_local};
pub use worker::{Worker, WorkerConfig};
