//! Where jobs live. The [`Worker`](crate::Worker) only needs these operations, so the desktop
//! drives it from SQLite ([`shorts_store::Store`]) and the cloud runner from PostgreSQL.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use shorts_domain::job::JobError;
use shorts_domain::{Job, JobId, JobKind};
use shorts_store::{Store, StoreError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QueueError {
    /// The domain refused the job transition, e.g. the caller no longer holds the lease.
    #[error(transparent)]
    Job(#[from] JobError),
    #[error("job queue: {0}")]
    Backend(String),
}

impl QueueError {
    /// The caller's lease is gone: the job was reclaimed, cancelled or finished elsewhere.
    pub fn is_lease_lost(&self) -> bool {
        matches!(
            self,
            Self::Job(JobError::LeaseNotHeld { .. } | JobError::NotAllowed { .. })
        )
    }
}

pub type QueueResult<T> = Result<T, QueueError>;

/// A job transition applied by [`JobQueue::modify`].
pub type JobChange<'a> = &'a (dyn Fn(&mut Job) -> Result<(), JobError> + Send + Sync);

/// Durable job storage with leases.
#[async_trait]
pub trait JobQueue: Send + Sync + 'static {
    /// Atomically claims the next due job (or one whose lease expired) for `owner`. Two
    /// claimers must never get the same job.
    async fn claim_next(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
        kinds: &[JobKind],
    ) -> QueueResult<Option<Job>>;

    async fn get(&self, id: JobId) -> QueueResult<Option<Job>>;

    /// Loads the job, applies `change` and writes it back with optimistic concurrency,
    /// re-reading and re-applying if another writer got there first.
    async fn modify(&self, id: JobId, change: JobChange<'_>) -> QueueResult<Job>;

    /// Startup recovery: jobs left running by an earlier process of `node` (lease owners
    /// `"{node}:..."` other than `current_owner`) go back to the queue with their checkpoints.
    async fn recover_interrupted(
        &self,
        node: &str,
        current_owner: &str,
        now: DateTime<Utc>,
    ) -> QueueResult<Vec<Job>>;

    /// The earliest `run_at` among queued jobs of `kinds` (all kinds when empty).
    async fn next_due_at(&self, kinds: &[JobKind]) -> QueueResult<Option<DateTime<Utc>>>;
}

fn store_error(e: StoreError) -> QueueError {
    match e {
        StoreError::Job(e) => QueueError::Job(e),
        other => QueueError::Backend(other.to_string()),
    }
}

/// The desktop's SQLite store.
#[async_trait]
impl JobQueue for Store {
    async fn claim_next(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
        kinds: &[JobKind],
    ) -> QueueResult<Option<Job>> {
        self.jobs()
            .claim_next(owner, now, ttl, kinds)
            .await
            .map_err(store_error)
    }

    async fn get(&self, id: JobId) -> QueueResult<Option<Job>> {
        self.jobs().get(id).await.map_err(store_error)
    }

    async fn modify(&self, id: JobId, change: JobChange<'_>) -> QueueResult<Job> {
        self.jobs()
            .modify(id, |job| change(job))
            .await
            .map_err(store_error)
    }

    async fn recover_interrupted(
        &self,
        node: &str,
        current_owner: &str,
        now: DateTime<Utc>,
    ) -> QueueResult<Vec<Job>> {
        self.jobs()
            .recover_interrupted(node, current_owner, now)
            .await
            .map_err(store_error)
    }

    async fn next_due_at(&self, kinds: &[JobKind]) -> QueueResult<Option<DateTime<Utc>>> {
        self.jobs().next_due_at(kinds).await.map_err(store_error)
    }
}
