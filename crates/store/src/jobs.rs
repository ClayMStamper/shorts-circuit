use chrono::{DateTime, Duration, Utc};
use shorts_domain::job::JobError;
use shorts_domain::{Job, JobId, JobKind, JobRecord, JobState, Lease, PauseReason};
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::codec::{RowExt, is_unique_violation, json, opt_json, opt_ts, to_i64, ts};
use crate::{StoreError, StoreResult};

#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub states: Vec<JobState>,
    pub kinds: Vec<JobKind>,
    pub subject: Option<String>,
    pub limit: Option<u32>,
}

pub struct Jobs<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str = "id, kind, subject, payload, state, run_at, time_sensitive, claims, \
                       failures, max_failures, lease_owner, lease_expires_at, heartbeat_at, \
                       checkpoint, last_error, pause_reason, version, created_at, updated_at";

/// How often [`Jobs::modify`] re-reads and retries after losing an optimistic-concurrency race.
const MODIFY_ATTEMPTS: usize = 5;

impl<'a> Jobs<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Enqueues a job. At most one queued/running/paused job may exist per (kind, subject).
    pub async fn insert(&self, job: &Job) -> StoreResult<()> {
        let r = job.record();
        let lease = r.lease.as_ref();
        let result = sqlx::query(&format!(
            "INSERT INTO jobs ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(r.id.to_string())
        .bind(r.kind.as_str())
        .bind(&r.subject)
        .bind(json(&r.payload))
        .bind(r.state.as_str())
        .bind(ts(r.run_at))
        .bind(r.time_sensitive)
        .bind(i64::from(r.claims))
        .bind(i64::from(r.failures))
        .bind(i64::from(r.max_failures))
        .bind(lease.map(|l| l.owner.clone()))
        .bind(opt_ts(lease.map(|l| l.expires_at)))
        .bind(opt_ts(lease.map(|l| l.heartbeat_at)))
        .bind(opt_json(r.checkpoint.as_ref()))
        .bind(&r.last_error)
        .bind(r.pause_reason.map(PauseReason::as_str))
        .bind(to_i64(r.version))
        .bind(ts(r.created_at))
        .bind(ts(r.updated_at))
        .execute(self.pool)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => Err(StoreError::DuplicateOpenJob {
                kind: r.kind.to_string(),
                subject: r.subject.clone(),
            }),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn get(&self, id: JobId) -> StoreResult<Option<Job>> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM jobs WHERE id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn require(&self, id: JobId) -> StoreResult<Job> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("job", id))
    }

    /// The queued, running or paused job for a subject, if any.
    pub async fn open_for(&self, kind: JobKind, subject: &str) -> StoreResult<Option<Job>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM jobs WHERE kind = ? AND subject = ? \
             AND state IN ('queued', 'running', 'paused')"
        ))
        .bind(kind.as_str())
        .bind(subject)
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    /// The most recently created job of `kind` for `subject`, in any state.
    pub async fn latest_for(&self, kind: JobKind, subject: &str) -> StoreResult<Option<Job>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM jobs WHERE kind = ? AND subject = ? \
             ORDER BY created_at DESC, id DESC LIMIT 1"
        ))
        .bind(kind.as_str())
        .bind(subject)
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    /// Most recently updated first.
    pub async fn list(&self, filter: &JobFilter) -> StoreResult<Vec<Job>> {
        let mut sql = format!("SELECT {COLUMNS} FROM jobs WHERE 1 = 1");
        let mut binds: Vec<String> = Vec::new();
        if !filter.states.is_empty() {
            sql.push_str(&format!(" AND state IN ({})", marks(filter.states.len())));
            binds.extend(filter.states.iter().map(|s| s.as_str().to_owned()));
        }
        if !filter.kinds.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", marks(filter.kinds.len())));
            binds.extend(filter.kinds.iter().map(|k| k.as_str().to_owned()));
        }
        if let Some(subject) = &filter.subject {
            sql.push_str(" AND subject = ?");
            binds.push(subject.clone());
        }
        sql.push_str(" ORDER BY updated_at DESC, id DESC");
        if let Some(limit) = filter.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let mut query = sqlx::query(&sql);
        for b in &binds {
            query = query.bind(b);
        }
        query
            .fetch_all(self.pool)
            .await?
            .iter()
            .map(decode)
            .collect()
    }

    /// The earliest `run_at` among queued jobs, so a worker knows how long it may sleep.
    pub async fn next_due_at(&self, kinds: &[JobKind]) -> StoreResult<Option<DateTime<Utc>>> {
        let mut sql = "SELECT MIN(run_at) FROM jobs WHERE state = 'queued'".to_owned();
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", marks(kinds.len())));
        }
        let mut query = sqlx::query_scalar::<_, Option<String>>(&sql);
        for k in kinds {
            query = query.bind(k.as_str());
        }
        let raw = query.fetch_one(self.pool).await?;
        raw.as_deref().map(crate::codec::parse_ts).transpose()
    }

    /// Writes `job` if the stored row is still at `expected_version`.
    pub async fn update(&self, job: &Job, expected_version: u64) -> StoreResult<()> {
        let mut conn = self.pool.acquire().await?;
        write(&mut conn, job, expected_version).await
    }

    /// Loads the job, applies `change` (a domain transition) and writes it back with optimistic
    /// concurrency, re-reading and re-applying if another writer got there first.
    pub async fn modify<F>(&self, id: JobId, mut change: F) -> StoreResult<Job>
    where
        F: FnMut(&mut Job) -> Result<(), JobError>,
    {
        for _ in 0..MODIFY_ATTEMPTS {
            let mut job = self.require(id).await?;
            let loaded = job.version();
            change(&mut job)?;
            match self.update(&job, loaded).await {
                Ok(()) => return Ok(job),
                Err(StoreError::Conflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(StoreError::conflict("job", id))
    }

    /// Atomically claims the next due job (or one whose lease expired) for `owner`.
    ///
    /// Runs in a `BEGIN IMMEDIATE` transaction, which takes SQLite's write lock up front, so two
    /// workers can never claim the same job; the version check is a second line of defence.
    /// Publish jobs for publications the cloud runner owns are never claimed here: this is the
    /// desktop's database, and the hand-over takes the same lock.
    pub async fn claim_next(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
        kinds: &[JobKind],
    ) -> StoreResult<Option<Job>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let now_s = ts(now);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM jobs WHERE ((state = 'queued' AND run_at <= ?1) \
             OR (state = 'running' AND (lease_expires_at IS NULL OR lease_expires_at <= ?1))) \
             AND NOT (kind = 'publish_publication' AND EXISTS (SELECT 1 FROM publications p \
             WHERE p.id = jobs.subject AND p.owner != 'desktop'))"
        );
        if !kinds.is_empty() {
            let marks = (0..kinds.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(" AND kind IN ({marks})"));
        }
        sql.push_str(" ORDER BY run_at, created_at LIMIT 1");
        let mut query = sqlx::query(&sql).bind(&now_s);
        for k in kinds {
            query = query.bind(k.as_str());
        }
        let Some(row) = query.fetch_optional(&mut *tx).await? else {
            tx.rollback().await?;
            return Ok(None);
        };
        let mut job = decode(&row)?;
        let loaded = job.version();
        job.claim(owner, now, ttl)?;
        write_tx(&mut tx, &job, loaded).await?;
        tx.commit().await?;
        Ok(Some(job))
    }

    /// Heartbeat. Fails with a lease error if the job was reclaimed, cancelled or finished.
    pub async fn renew(
        &self,
        id: JobId,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> StoreResult<Job> {
        self.modify(id, |job| job.renew(owner, now, ttl)).await
    }

    pub async fn save_checkpoint(
        &self,
        id: JobId,
        owner: &str,
        checkpoint: serde_json::Value,
        now: DateTime<Utc>,
    ) -> StoreResult<Job> {
        self.modify(id, |job| {
            job.save_checkpoint(owner, checkpoint.clone(), now)
        })
        .await
    }

    /// Startup recovery for a single execution owner: every job still `running` under a lease
    /// from an earlier process of `node` (owner strings `"{node}:..."`) other than
    /// `current_owner` goes back to the queue with its checkpoint intact.
    pub async fn recover_interrupted(
        &self,
        node: &str,
        current_owner: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<Vec<Job>> {
        let pattern = format!(
            "{}:%",
            node.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM jobs WHERE state = 'running' \
             AND lease_owner LIKE ? ESCAPE '\\' AND lease_owner != ?"
        ))
        .bind(pattern)
        .bind(current_owner)
        .fetch_all(self.pool)
        .await?;
        let mut recovered = Vec::new();
        for row in rows {
            let mut job = decode(&row)?;
            let loaded = job.version();
            job.recover_interrupted(now)?;
            match self.update(&job, loaded).await {
                Ok(()) => recovered.push(job),
                // Someone else already moved it on; nothing to recover.
                Err(StoreError::Conflict { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(recovered)
    }
}

/// The open job of `kind` for `subject`, read inside a transaction.
pub(crate) async fn open_for_tx(
    tx: &mut Transaction<'_, Sqlite>,
    kind: JobKind,
    subject: &str,
) -> StoreResult<Option<Job>> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE kind = ? AND subject = ? \
         AND state IN ('queued', 'running', 'paused')"
    ))
    .bind(kind.as_str())
    .bind(subject)
    .fetch_optional(&mut **tx)
    .await?
    .as_ref()
    .map(decode)
    .transpose()
}

fn marks(n: usize) -> String {
    vec!["?"; n].join(", ")
}

pub(crate) async fn write_tx(
    tx: &mut Transaction<'_, Sqlite>,
    job: &Job,
    expected_version: u64,
) -> StoreResult<()> {
    write(&mut *tx, job, expected_version).await
}

async fn write(
    conn: &mut sqlx::SqliteConnection,
    job: &Job,
    expected_version: u64,
) -> StoreResult<()> {
    let r = job.record();
    let lease = r.lease.as_ref();
    let result = sqlx::query(
        "UPDATE jobs SET state = ?, run_at = ?, claims = ?, failures = ?, lease_owner = ?, \
         lease_expires_at = ?, heartbeat_at = ?, checkpoint = ?, last_error = ?, pause_reason = ?, \
         version = ?, updated_at = ? WHERE id = ? AND version = ?",
    )
    .bind(r.state.as_str())
    .bind(ts(r.run_at))
    .bind(i64::from(r.claims))
    .bind(i64::from(r.failures))
    .bind(lease.map(|l| l.owner.clone()))
    .bind(opt_ts(lease.map(|l| l.expires_at)))
    .bind(opt_ts(lease.map(|l| l.heartbeat_at)))
    .bind(opt_json(r.checkpoint.as_ref()))
    .bind(&r.last_error)
    .bind(r.pause_reason.map(PauseReason::as_str))
    .bind(to_i64(r.version))
    .bind(ts(r.updated_at))
    .bind(r.id.to_string())
    .bind(to_i64(expected_version))
    .execute(&mut *conn)
    .await;
    match result {
        Ok(done) if done.rows_affected() == 1 => Ok(()),
        Ok(_) => Err(StoreError::conflict("job", r.id)),
        Err(e) if is_unique_violation(&e) => Err(StoreError::DuplicateOpenJob {
            kind: r.kind.to_string(),
            subject: r.subject.clone(),
        }),
        Err(e) => Err(e.into()),
    }
}

fn decode(row: &SqliteRow) -> StoreResult<Job> {
    let kind = row.text("kind")?;
    let state = row.text("state")?;
    let lease = match (
        row.opt_text("lease_owner")?,
        row.opt_time("lease_expires_at")?,
        row.opt_time("heartbeat_at")?,
    ) {
        (Some(owner), Some(expires_at), Some(heartbeat_at)) => Some(Lease {
            owner,
            expires_at,
            heartbeat_at,
        }),
        (None, None, None) => None,
        _ => return Err(StoreError::Corrupt("partial lease columns".into())),
    };
    Ok(Job::from_record(JobRecord {
        id: row.parsed("id")?,
        kind: JobKind::parse(&kind)
            .ok_or_else(|| StoreError::Corrupt(format!("bad job kind {kind:?}")))?,
        subject: row.text("subject")?,
        payload: row.json("payload")?,
        state: JobState::parse(&state)
            .ok_or_else(|| StoreError::Corrupt(format!("bad job state {state:?}")))?,
        run_at: row.time("run_at")?,
        time_sensitive: row.int("time_sensitive")? != 0,
        claims: row.u32("claims")?,
        failures: row.u32("failures")?,
        max_failures: row.u32("max_failures")?,
        lease,
        checkpoint: row.opt_json("checkpoint")?,
        last_error: row.opt_text("last_error")?,
        pause_reason: row
            .opt_text("pause_reason")?
            .map(|r| {
                PauseReason::parse(&r)
                    .ok_or_else(|| StoreError::Corrupt(format!("bad pause reason {r:?}")))
            })
            .transpose()?,
        version: row.u64("version")?,
        created_at: row.time("created_at")?,
        updated_at: row.time("updated_at")?,
    }))
}
