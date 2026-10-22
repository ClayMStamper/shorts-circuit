//! The contract between the worker and the code that does the work.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use shorts_domain::{Job, JobId, PauseReason};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::queue::JobQueue;

/// Why a handler stopped without finishing. The worker maps each kind to a disposition.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum JobFailure {
    /// Worth retrying: network errors, 5xx responses, a busy encoder. `retry_after` is a floor
    /// for the next attempt if the provider gave one.
    #[error("transient failure: {message}")]
    Transient {
        message: String,
        retry_after: Option<Duration>,
    },
    /// The provider asked us to slow down. Retried after `retry_after` without counting
    /// against the retry budget.
    #[error("rate limited for {retry_after:?}: {message}")]
    RateLimited {
        message: String,
        retry_after: Duration,
    },
    /// Credentials expired or were revoked; pause until the user reconnects.
    #[error("authentication required: {message}")]
    AuthRequired { message: String },
    /// The submission is invalid; pause until the user fixes it.
    #[error("invalid submission: {message}")]
    Invalid { message: String },
    /// A publish request may or may not have been accepted. Never retried automatically.
    #[error("outcome unknown: {message}")]
    OutcomeUnknown { message: String },
    /// Retrying won't help.
    #[error("failed: {message}")]
    Permanent { message: String },
    /// The job's subject no longer needs this job (cancelled, revised, already done).
    #[error("abandoned: {message}")]
    Abandon { message: String },
    /// The worker no longer holds the lease; stop without writing anything.
    #[error("lease lost")]
    LeaseLost,
}

impl JobFailure {
    pub fn transient(message: impl Into<String>) -> Self {
        Self::Transient {
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self::Permanent {
            message: message.into(),
        }
    }
}

/// What the worker decided to do with a job after running it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Succeeded,
    RetryAt {
        at: DateTime<Utc>,
        error: String,
        rate_limited: bool,
    },
    Paused {
        reason: PauseReason,
        message: String,
    },
    Failed {
        message: String,
    },
    /// Overdue under the expiry policy; the handler did not run.
    Expired {
        due: DateTime<Utc>,
    },
    Cancelled {
        message: String,
    },
}

/// Executes one kind of job.
#[async_trait]
pub trait JobHandler: Send + Sync + 'static {
    /// Does (or resumes) the work. Long-running handlers should save checkpoints after each
    /// meaningful step and may check [`JobContext::is_cancelled`] between steps.
    async fn run(&self, job: &Job, ctx: &JobContext) -> Result<(), JobFailure>;

    /// Called with the worker's decision before the job row is updated, so handlers can mirror
    /// it onto their subject (e.g. move a publication to `RetryScheduled`). Not called when the
    /// lease was lost.
    async fn on_settled(&self, _job: &Job, _disposition: &Disposition) {}
}

/// Per-run access to the job's checkpoint and lease.
pub struct JobContext {
    queue: Arc<dyn JobQueue>,
    job_id: JobId,
    owner: String,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    checkpoint: Mutex<Option<serde_json::Value>>,
    attempt: u32,
}

impl JobContext {
    pub(crate) fn new(
        queue: Arc<dyn JobQueue>,
        job: &Job,
        owner: String,
        clock: Arc<dyn Clock>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            queue,
            job_id: job.id(),
            owner,
            clock,
            cancel,
            checkpoint: Mutex::new(job.checkpoint().cloned()),
            attempt: job.claims(),
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// 1 on the first run, higher after retries or recovery.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The last checkpoint saved for this job (by this run or an interrupted earlier one).
    pub fn checkpoint<T: DeserializeOwned>(&self) -> Option<T> {
        let guard = self.checkpoint.lock().expect("checkpoint lock");
        guard
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Persists a checkpoint. Fails with [`JobFailure::LeaseLost`] if another worker took over.
    pub async fn save_checkpoint<T: Serialize>(&self, value: &T) -> Result<(), JobFailure> {
        let json = serde_json::to_value(value)
            .map_err(|e| JobFailure::permanent(format!("checkpoint is not serializable: {e}")))?;
        let now = self.clock.now();
        let owner = self.owner.as_str();
        match self
            .queue
            .modify(self.job_id, &|j| {
                j.save_checkpoint(owner, json.clone(), now)
            })
            .await
        {
            Ok(_) => {
                *self.checkpoint.lock().expect("checkpoint lock") = Some(json);
                Ok(())
            }
            Err(e) if e.is_lease_lost() => {
                self.cancel.cancel();
                Err(JobFailure::LeaseLost)
            }
            Err(e) => Err(JobFailure::transient(format!("saving checkpoint: {e}"))),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Resolves when the lease is lost or the worker shuts down.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }
}
