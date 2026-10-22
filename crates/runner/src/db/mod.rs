//! PostgreSQL persistence for the runner. Like the desktop's SQLite store it owns all SQL and
//! persists domain values as they are; transitions are decided by `shorts-domain`. Mutable rows
//! (publications, jobs) are written with optimistic concurrency on a version column, and the
//! ownership-sensitive operations (claiming, releasing, receiving a hand-over) lock the rows
//! they read.

mod accounts;
mod installations;
mod jobs;
mod publications;
mod uploads;

use std::sync::Arc;
use std::time::Duration;

use shorts_domain::job::JobError;
use shorts_domain::{InFlight, TransitionError};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use thiserror::Error;

pub use accounts::StoredCredential;
pub use installations::Installation;
pub use publications::{HeldPublication, Received};

use crate::vault::{Vault, VaultError};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("{entity} {id} not found")]
    NotFound { entity: &'static str, id: String },
    /// Someone else changed the row since it was read.
    #[error("{entity} {id} was modified concurrently")]
    Conflict { entity: &'static str, id: String },
    /// The request contradicts what the runner holds.
    #[error("{0}")]
    Refused(String),
    /// A hand-over the desktop already took back.
    #[error("that hand-over was already taken back")]
    HandoverGone,
    #[error("the video for this hand-over hasn't been uploaded completely")]
    MediaIncomplete,
    /// The runner rotated the credential since the desktop last read it.
    #[error("the runner has a newer credential (revision {revision})")]
    CredentialConflict { revision: u64 },
    #[error("the upload is at byte {received}")]
    OffsetMismatch { received: u64 },
    #[error(transparent)]
    Job(#[from] JobError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
}

impl DbError {
    pub(crate) fn not_found(entity: &'static str, id: impl ToString) -> Self {
        Self::NotFound {
            entity,
            id: id.to_string(),
        }
    }

    pub(crate) fn conflict(entity: &'static str, id: impl ToString) -> Self {
        Self::Conflict {
            entity,
            id: id.to_string(),
        }
    }

    /// The ownership change was refused because the cloud attempt is in flight.
    pub fn in_flight(&self) -> Option<InFlight> {
        match self {
            Self::Transition(TransitionError::InFlight(why)) => Some(*why),
            _ => None,
        }
    }
}

pub type DbResult<T> = Result<T, DbError>;

/// Handle to the runner database. Cheap to clone.
#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    vault: Arc<Vault>,
}

impl std::fmt::Debug for PgStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgStore").finish_non_exhaustive()
    }
}

impl PgStore {
    /// Connects to `options` and runs migrations.
    pub async fn connect(options: PgConnectOptions, vault: Arc<Vault>) -> DbResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool, vault })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// Ends a transaction: commits on success; on an error rolls back right away. (A dropped
/// transaction is only rolled back when its connection is next used, so its row locks could
/// outlive the call and make a claim skip a job it should take.)
pub(crate) async fn finish<T>(
    tx: sqlx::Transaction<'_, sqlx::Postgres>,
    result: DbResult<T>,
) -> DbResult<T> {
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(e) => {
            if let Err(rollback) = tx.rollback().await {
                tracing::warn!(error = %rollback, "rollback failed");
            }
            Err(e)
        }
    }
}

pub(crate) fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

pub(crate) fn to_u64(v: i64, what: &str) -> DbResult<u64> {
    u64::try_from(v).map_err(|_| DbError::Corrupt(format!("bad {what}: {v}")))
}

pub(crate) fn to_u32(v: i32, what: &str) -> DbResult<u32> {
    u32::try_from(v).map_err(|_| DbError::Corrupt(format!("bad {what}: {v}")))
}

pub(crate) fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_unique_violation())
}
