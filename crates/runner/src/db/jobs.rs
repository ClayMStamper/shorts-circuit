//! Jobs, with the same lease columns and rules as the desktop's SQLite store.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use shorts_domain::{Job, JobId, JobKind, JobRecord, JobState, Lease, PauseReason};
use shorts_scheduler::{JobChange, JobQueue, QueueError, QueueResult};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgConnection, Row};

use super::{DbError, DbResult, PgStore, is_unique_violation, to_i64, to_u32, to_u64};

const COLUMNS: &str = "id, kind, subject, payload, state, run_at, time_sensitive, claims, \
                       failures, max_failures, lease_owner, lease_expires_at, heartbeat_at, \
                       checkpoint, last_error, pause_reason, version, created_at, updated_at";

/// How often [`JobQueue::modify`] re-reads and retries after losing an optimistic race.
const MODIFY_ATTEMPTS: usize = 5;

pub(crate) fn decode(row: &PgRow) -> DbResult<Job> {
    let kind: String = row.try_get("kind")?;
    let state: String = row.try_get("state")?;
    let lease = match (
        row.try_get::<Option<String>, _>("lease_owner")?,
        row.try_get::<Option<DateTime<Utc>>, _>("lease_expires_at")?,
        row.try_get::<Option<DateTime<Utc>>, _>("heartbeat_at")?,
    ) {
        (Some(owner), Some(expires_at), Some(heartbeat_at)) => Some(Lease {
            owner,
            expires_at,
            heartbeat_at,
        }),
        (None, None, None) => None,
        _ => return Err(DbError::Corrupt("partial lease columns".into())),
    };
    let pause_reason: Option<String> = row.try_get("pause_reason")?;
    let payload: Json<serde_json::Value> = row.try_get("payload")?;
    let checkpoint: Option<Json<serde_json::Value>> = row.try_get("checkpoint")?;
    let id: uuid::Uuid = row.try_get("id")?;
    Ok(Job::from_record(JobRecord {
        id: JobId::from_uuid(id),
        kind: JobKind::parse(&kind)
            .ok_or_else(|| DbError::Corrupt(format!("bad job kind {kind:?}")))?,
        subject: row.try_get("subject")?,
        payload: payload.0,
        state: JobState::parse(&state)
            .ok_or_else(|| DbError::Corrupt(format!("bad job state {state:?}")))?,
        run_at: row.try_get("run_at")?,
        time_sensitive: row.try_get("time_sensitive")?,
        claims: to_u32(row.try_get("claims")?, "claims")?,
        failures: to_u32(row.try_get("failures")?, "failures")?,
        max_failures: to_u32(row.try_get("max_failures")?, "max_failures")?,
        lease,
        checkpoint: checkpoint.map(|c| c.0),
        last_error: row.try_get("last_error")?,
        pause_reason: pause_reason
            .map(|r| {
                PauseReason::parse(&r)
                    .ok_or_else(|| DbError::Corrupt(format!("bad pause reason {r:?}")))
            })
            .transpose()?,
        version: to_u64(row.try_get("version")?, "version")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    }))
}

fn i32_of(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

pub(crate) async fn insert(conn: &mut PgConnection, job: &Job) -> DbResult<()> {
    let r = job.record();
    let lease = r.lease.as_ref();
    let result = sqlx::query(&format!(
        "INSERT INTO jobs ({COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
         $12, $13, $14, $15, $16, $17, $18, $19)"
    ))
    .bind(r.id.as_uuid())
    .bind(r.kind.as_str())
    .bind(&r.subject)
    .bind(Json(&r.payload))
    .bind(r.state.as_str())
    .bind(r.run_at)
    .bind(r.time_sensitive)
    .bind(i32_of(r.claims))
    .bind(i32_of(r.failures))
    .bind(i32_of(r.max_failures))
    .bind(lease.map(|l| l.owner.clone()))
    .bind(lease.map(|l| l.expires_at))
    .bind(lease.map(|l| l.heartbeat_at))
    .bind(r.checkpoint.as_ref().map(Json))
    .bind(&r.last_error)
    .bind(r.pause_reason.map(PauseReason::as_str))
    .bind(to_i64(r.version))
    .bind(r.created_at)
    .bind(r.updated_at)
    .execute(conn)
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(e) if is_unique_violation(&e) => Err(DbError::Refused(format!(
            "an open {} job already exists for {}",
            r.kind, r.subject
        ))),
        Err(e) => Err(e.into()),
    }
}

/// Writes `job` if the stored row is still at `expected_version`.
pub(crate) async fn write(
    conn: &mut PgConnection,
    job: &Job,
    expected_version: u64,
) -> DbResult<()> {
    let r = job.record();
    let lease = r.lease.as_ref();
    let done = sqlx::query(
        "UPDATE jobs SET state = $1, run_at = $2, claims = $3, failures = $4, lease_owner = $5, \
         lease_expires_at = $6, heartbeat_at = $7, checkpoint = $8, last_error = $9, \
         pause_reason = $10, version = $11, updated_at = $12 WHERE id = $13 AND version = $14",
    )
    .bind(r.state.as_str())
    .bind(r.run_at)
    .bind(i32_of(r.claims))
    .bind(i32_of(r.failures))
    .bind(lease.map(|l| l.owner.clone()))
    .bind(lease.map(|l| l.expires_at))
    .bind(lease.map(|l| l.heartbeat_at))
    .bind(r.checkpoint.as_ref().map(Json))
    .bind(&r.last_error)
    .bind(r.pause_reason.map(PauseReason::as_str))
    .bind(to_i64(r.version))
    .bind(r.updated_at)
    .bind(r.id.as_uuid())
    .bind(to_i64(expected_version))
    .execute(conn)
    .await?;
    if done.rows_affected() == 1 {
        Ok(())
    } else {
        Err(DbError::conflict("job", r.id))
    }
}

/// The open publish job for a publication, locked for the rest of the transaction.
pub(crate) async fn open_for_update(
    conn: &mut PgConnection,
    subject: &str,
) -> DbResult<Option<Job>> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE kind = 'publish_publication' AND subject = $1 \
         AND state IN ('queued', 'running', 'paused') FOR UPDATE"
    ))
    .bind(subject)
    .fetch_optional(conn)
    .await?
    .as_ref()
    .map(decode)
    .transpose()
}

/// The most recent publish job for a publication, in any state.
pub(crate) async fn latest_for(conn: &mut PgConnection, subject: &str) -> DbResult<Option<Job>> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE kind = 'publish_publication' AND subject = $1 \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    ))
    .bind(subject)
    .fetch_optional(conn)
    .await?
    .as_ref()
    .map(decode)
    .transpose()
}

impl PgStore {
    pub async fn job(&self, id: JobId) -> DbResult<Option<Job>> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM jobs WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn latest_job(&self, subject: &str) -> DbResult<Option<Job>> {
        let mut conn = self.pool.acquire().await?;
        latest_for(&mut conn, subject).await
    }

    pub async fn modify_job(
        &self,
        id: JobId,
        change: impl Fn(&mut Job) -> Result<(), shorts_domain::job::JobError>,
    ) -> DbResult<Job> {
        for _ in 0..MODIFY_ATTEMPTS {
            let mut job = self
                .job(id)
                .await?
                .ok_or_else(|| DbError::not_found("job", id))?;
            let loaded = job.version();
            change(&mut job)?;
            let mut conn = self.pool.acquire().await?;
            match write(&mut conn, &job, loaded).await {
                Ok(()) => return Ok(job),
                Err(DbError::Conflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(DbError::conflict("job", id))
    }

    /// Claims the next due job whose publication the cloud owns. `FOR UPDATE SKIP LOCKED`
    /// keeps two claimers apart, and a release holding the job row makes the claim skip it.
    async fn claim(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
        kinds: &[JobKind],
    ) -> DbResult<Option<Job>> {
        let mut tx = self.pool.begin().await?;
        let mut sql = format!(
            "SELECT {COLUMNS} FROM jobs j WHERE ((j.state = 'queued' AND j.run_at <= $1) \
             OR (j.state = 'running' AND (j.lease_expires_at IS NULL OR j.lease_expires_at <= $1))) \
             AND (j.kind <> 'publish_publication' OR EXISTS (SELECT 1 FROM publications p \
             WHERE p.id::text = j.subject AND p.owner = 'cloud'))"
        );
        if !kinds.is_empty() {
            sql.push_str(" AND j.kind = ANY($2)");
        }
        sql.push_str(" ORDER BY j.run_at, j.created_at LIMIT 1 FOR UPDATE OF j SKIP LOCKED");
        let mut query = sqlx::query(&sql).bind(now);
        if !kinds.is_empty() {
            query = query.bind(
                kinds
                    .iter()
                    .map(|k| k.as_str().to_owned())
                    .collect::<Vec<_>>(),
            );
        }
        let Some(row) = query.fetch_optional(&mut *tx).await? else {
            tx.rollback().await?;
            return Ok(None);
        };
        let mut job = decode(&row)?;
        let loaded = job.version();
        job.claim(owner, now, ttl)?;
        write(&mut tx, &job, loaded).await?;
        tx.commit().await?;
        Ok(Some(job))
    }

    async fn recover(
        &self,
        node: &str,
        current_owner: &str,
        now: DateTime<Utc>,
    ) -> DbResult<Vec<Job>> {
        let pattern = format!(
            "{}:%",
            node.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM jobs WHERE state = 'running' \
             AND lease_owner LIKE $1 ESCAPE '\\' AND lease_owner <> $2"
        ))
        .bind(pattern)
        .bind(current_owner)
        .fetch_all(&self.pool)
        .await?;
        let mut recovered = Vec::new();
        for row in rows {
            let mut job = decode(&row)?;
            let loaded = job.version();
            job.recover_interrupted(now)?;
            let mut conn = self.pool.acquire().await?;
            match write(&mut conn, &job, loaded).await {
                Ok(()) => recovered.push(job),
                Err(DbError::Conflict { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(recovered)
    }

    async fn next_due(&self, kinds: &[JobKind]) -> DbResult<Option<DateTime<Utc>>> {
        let mut sql = "SELECT MIN(run_at) FROM jobs WHERE state = 'queued'".to_owned();
        if !kinds.is_empty() {
            sql.push_str(" AND kind = ANY($1)");
        }
        let mut query = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(&sql);
        if !kinds.is_empty() {
            query = query.bind(
                kinds
                    .iter()
                    .map(|k| k.as_str().to_owned())
                    .collect::<Vec<_>>(),
            );
        }
        Ok(query.fetch_one(&self.pool).await?)
    }
}

fn queue_error(e: DbError) -> QueueError {
    match e {
        DbError::Job(e) => QueueError::Job(e),
        other => QueueError::Backend(other.to_string()),
    }
}

#[async_trait]
impl JobQueue for PgStore {
    async fn claim_next(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
        kinds: &[JobKind],
    ) -> QueueResult<Option<Job>> {
        self.claim(owner, now, ttl, kinds)
            .await
            .map_err(queue_error)
    }

    async fn get(&self, id: JobId) -> QueueResult<Option<Job>> {
        self.job(id).await.map_err(queue_error)
    }

    async fn modify(&self, id: JobId, change: JobChange<'_>) -> QueueResult<Job> {
        self.modify_job(id, change).await.map_err(queue_error)
    }

    async fn recover_interrupted(
        &self,
        node: &str,
        current_owner: &str,
        now: DateTime<Utc>,
    ) -> QueueResult<Vec<Job>> {
        self.recover(node, current_owner, now)
            .await
            .map_err(queue_error)
    }

    async fn next_due_at(&self, kinds: &[JobKind]) -> QueueResult<Option<DateTime<Utc>>> {
        self.next_due(kinds).await.map_err(queue_error)
    }
}
