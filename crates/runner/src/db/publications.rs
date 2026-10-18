//! Publications the desktop handed over, their immutable revisions, audit log and attempts.

use chrono::{DateTime, NaiveDate, Utc};
use shorts_cloud::{HandoverPut, MediaRef, PublicationStatus};
use shorts_domain::{
    AccountId, AttemptId, Execution, ExecutionOwner, HandoverId, Job, JobState, Publication,
    PublicationEvent, PublicationId, PublicationRecord, PublicationState, PublishAttempt,
    Transition,
};
use shorts_publish::{PublishCheckpoint, publish_job};
use sqlx::types::Json;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{DbError, DbResult, PgStore, jobs, to_i64};

/// Transient failures a cloud publish job may have before it fails.
pub const PUBLISH_MAX_FAILURES: u32 = 6;
const APPLY_ATTEMPTS: usize = 5;

/// A publication as the runner holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldPublication {
    pub installation: Uuid,
    /// The latest hand-over, also after the publication was given back.
    pub handover: HandoverId,
    pub publication: Publication,
    pub media: MediaRef,
}

/// What receiving a hand-over did.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// A new hand-over: stored, with a publish job at its posting time.
    New(HeldPublication),
    /// The same hand-over again: nothing changed.
    Unchanged(HeldPublication),
}

impl Received {
    pub fn held(&self) -> &HeldPublication {
        match self {
            Self::New(h) | Self::Unchanged(h) => h,
        }
    }
}

fn decode(row: &sqlx::postgres::PgRow) -> DbResult<HeldPublication> {
    let record: Json<PublicationRecord> = row.try_get("record")?;
    let media: Json<MediaRef> = row.try_get("media")?;
    Ok(HeldPublication {
        installation: row.try_get("installation_id")?,
        handover: HandoverId::from_uuid(row.try_get("handover_id")?),
        publication: Publication::from_record(record.0),
        media: media.0,
    })
}

async fn get(
    conn: &mut PgConnection,
    id: PublicationId,
    lock: bool,
) -> DbResult<Option<HeldPublication>> {
    let sql = format!(
        "SELECT installation_id, handover_id, record, media FROM publications WHERE id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query(&sql)
        .bind(id.as_uuid())
        .fetch_optional(conn)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
}

async fn log(
    conn: &mut PgConnection,
    id: PublicationId,
    from: Option<PublicationState>,
    to: PublicationState,
    event: &str,
    revision: u32,
    at: DateTime<Utc>,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO publication_events (publication_id, from_state, to_state, event, revision, at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id.as_uuid())
    .bind(from.map(|s| s.as_str()))
    .bind(to.as_str())
    .bind(event)
    .bind(i32::try_from(revision).unwrap_or(i32::MAX))
    .bind(at)
    .execute(conn)
    .await?;
    Ok(())
}

/// Writes the result of `Publication::apply` if the row is still at the version the
/// transition started from.
async fn save(
    conn: &mut PgConnection,
    publication: &Publication,
    transition: &Transition,
) -> DbResult<()> {
    let r = publication.record();
    if transition.publication_id != r.id || transition.to != r.state {
        return Err(DbError::Corrupt(
            "transition does not belong to this publication state".into(),
        ));
    }
    let expected = r
        .version
        .checked_sub(1)
        .ok_or_else(|| DbError::Corrupt("saved publication must come from a transition".into()))?;
    let done = sqlx::query(
        "UPDATE publications SET owner = $2, state = $3, revision = $4, record = $5, \
         version = $6, updated_at = $7 WHERE id = $1 AND version = $8",
    )
    .bind(r.id.as_uuid())
    .bind(r.owner.as_str())
    .bind(r.state.as_str())
    .bind(i32::try_from(r.revision.number).unwrap_or(i32::MAX))
    .bind(Json(r))
    .bind(to_i64(r.version))
    .bind(r.updated_at)
    .bind(to_i64(expected))
    .execute(&mut *conn)
    .await?;
    if done.rows_affected() == 0 {
        return Err(DbError::conflict("publication", r.id));
    }
    log(
        conn,
        r.id,
        Some(transition.from),
        transition.to,
        transition.event,
        transition.revision,
        transition.at,
    )
    .await
}

fn reached_provider(job: Option<&Job>) -> bool {
    job.is_some_and(|j| PublishCheckpoint::of(j).reached_provider())
}

/// The status the desktop reads: the owner report plus upload progress.
pub(crate) fn status(held: &HeldPublication, job: Option<&Job>) -> PublicationStatus {
    let p = &held.publication;
    let report = p.owner_report(held.handover, reached_provider(job));
    let progress = job
        .filter(|_| p.state().is_active())
        .and_then(|j| PublishCheckpoint::of(j).progress().cloned());
    PublicationStatus {
        report,
        progress_fraction: progress.as_ref().and_then(|g| g.fraction),
        progress_detail: progress.and_then(|g| g.detail),
    }
}

impl PgStore {
    /// Stores a hand-over: the approved revision (immutable), the publication and a publish
    /// job at its posting time. Idempotent for the same hand-over; refused when the runner
    /// holds the publication under another live hand-over, and gone when this one was already
    /// taken back.
    pub async fn receive_handover(
        &self,
        installation: Uuid,
        put: &HandoverPut,
        now: DateTime<Utc>,
    ) -> DbResult<Received> {
        let publication = Publication::received_from_desktop(put.publication.clone())?;
        if publication.handover() != Some(put.handover) {
            return Err(DbError::Refused(
                "the publication was handed over under another id".into(),
            ));
        }
        let id = publication.id();
        let account = publication.content().destination.account_id;
        let mut tx = self.pool.begin().await?;

        if let Some(existing) = get(&mut tx, id, true).await? {
            if existing.installation != installation {
                return Err(DbError::not_found("publication", id));
            }
            let held = existing.handover == put.handover;
            match (held, existing.publication.owner()) {
                (true, ExecutionOwner::Cloud) => {
                    tx.rollback().await?;
                    return Ok(Received::Unchanged(existing));
                }
                (true, ExecutionOwner::Desktop) => return Err(DbError::HandoverGone),
                (false, ExecutionOwner::Cloud) => {
                    return Err(DbError::Refused(
                        "the runner is already publishing this under an earlier hand-over; take \
                         that one back first"
                            .into(),
                    ));
                }
                // Taken back earlier and now handed over again.
                (false, ExecutionOwner::Desktop) => {}
            }
        }

        let account_installation: Option<Uuid> =
            sqlx::query_scalar("SELECT installation_id FROM accounts WHERE id = $1")
                .bind(account.as_uuid())
                .fetch_optional(&mut *tx)
                .await?;
        if account_installation != Some(installation) {
            return Err(DbError::Refused(
                "send the account and its credential before the publication".into(),
            ));
        }
        let complete: Option<bool> = sqlx::query_scalar(
            "SELECT complete FROM uploads WHERE installation_id = $1 AND sha256 = $2",
        )
        .bind(installation)
        .bind(&put.media.sha256)
        .fetch_optional(&mut *tx)
        .await?;
        if complete != Some(true) {
            return Err(DbError::MediaIncomplete);
        }

        let revision = publication.revision();
        let stored: Option<Json<shorts_domain::RevisionContent>> = sqlx::query_scalar(
            "SELECT content FROM publication_revisions WHERE publication_id = $1 AND number = $2",
        )
        .bind(id.as_uuid())
        .bind(i32::try_from(revision.number).unwrap_or(i32::MAX))
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(content) = &stored
            && content.0 != revision.content
        {
            return Err(DbError::Refused(format!(
                "revision {} was handed over before with different content",
                revision.number
            )));
        }

        let r = publication.record();
        sqlx::query(
            "INSERT INTO publications (id, installation_id, account_id, handover_id, owner, state, \
             revision, record, media, media_sha256, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12) \
             ON CONFLICT (id) DO UPDATE SET account_id = EXCLUDED.account_id, \
             handover_id = EXCLUDED.handover_id, owner = EXCLUDED.owner, state = EXCLUDED.state, \
             revision = EXCLUDED.revision, record = EXCLUDED.record, media = EXCLUDED.media, \
             media_sha256 = EXCLUDED.media_sha256, version = EXCLUDED.version, \
             updated_at = EXCLUDED.updated_at",
        )
        .bind(id.as_uuid())
        .bind(installation)
        .bind(account.as_uuid())
        .bind(put.handover.as_uuid())
        .bind(r.owner.as_str())
        .bind(r.state.as_str())
        .bind(i32::try_from(revision.number).unwrap_or(i32::MAX))
        .bind(Json(r))
        .bind(Json(&put.media))
        .bind(&put.media.sha256)
        .bind(to_i64(r.version))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        // Revisions reference the publication row, so they are inserted after it.
        if stored.is_none() {
            sqlx::query(
                "INSERT INTO publication_revisions (publication_id, number, content, received_at) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(id.as_uuid())
            .bind(i32::try_from(revision.number).unwrap_or(i32::MAX))
            .bind(Json(&revision.content))
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        log(&mut tx, id, None, r.state, "received", revision.number, now).await?;
        let at = publication
            .due_at()
            .ok_or_else(|| DbError::Corrupt("a handed-over publication has no time".into()))?;
        jobs::insert(
            &mut tx,
            &publish_job(&publication, at, PUBLISH_MAX_FAILURES, now),
        )
        .await?;
        tx.commit().await?;
        Ok(Received::New(HeldPublication {
            installation,
            handover: put.handover,
            publication,
            media: put.media.clone(),
        }))
    }

    pub async fn publication(&self, id: PublicationId) -> DbResult<Option<HeldPublication>> {
        let mut conn = self.pool.acquire().await?;
        get(&mut conn, id, false).await
    }

    /// Applies an event built from the current state, with optimistic concurrency (re-reading
    /// on conflict).
    pub async fn apply(
        &self,
        id: PublicationId,
        event: impl Fn(&Publication) -> PublicationEvent,
        now: DateTime<Utc>,
    ) -> DbResult<Publication> {
        for _ in 0..APPLY_ATTEMPTS {
            let mut conn = self.pool.acquire().await?;
            let mut p = get(&mut conn, id, false)
                .await?
                .ok_or_else(|| DbError::not_found("publication", id))?
                .publication;
            let transition = p.apply(event(&p), now)?;
            let mut tx = self.pool.begin().await?;
            match save(&mut tx, &p, &transition).await {
                Ok(()) => {
                    tx.commit().await?;
                    return Ok(p);
                }
                Err(DbError::Conflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(DbError::conflict("publication", id))
    }

    /// The installation's publications: everything still cloud-owned, plus those given back
    /// or published in the last 30 days.
    pub async fn statuses(
        &self,
        installation: Uuid,
        now: DateTime<Utc>,
    ) -> DbResult<Vec<PublicationStatus>> {
        let rows = sqlx::query(
            "SELECT installation_id, handover_id, record, media FROM publications WHERE installation_id = $1 \
             AND (owner = 'cloud' OR updated_at > $2) ORDER BY created_at",
        )
        .bind(installation)
        .bind(now - chrono::Duration::days(30))
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        let mut conn = self.pool.acquire().await?;
        for row in rows {
            let held = decode(&row)?;
            let job = jobs::latest_for(&mut conn, &held.publication.id().to_string()).await?;
            out.push(status(&held, job.as_ref()));
        }
        Ok(out)
    }

    pub async fn status(
        &self,
        installation: Uuid,
        id: PublicationId,
    ) -> DbResult<Option<PublicationStatus>> {
        let mut conn = self.pool.acquire().await?;
        let Some(held) = get(&mut conn, id, false)
            .await?
            .filter(|h| h.installation == installation)
        else {
            return Ok(None);
        };
        let job = jobs::latest_for(&mut conn, &id.to_string()).await?;
        Ok(Some(status(&held, job.as_ref())))
    }

    /// Gives a publication back to the desktop. Locks the publication and its open job, so a
    /// worker can't claim the job meanwhile (or, if it claimed first, the release sees it
    /// running and is refused). Releasing an already released hand-over answers the same.
    pub async fn release(
        &self,
        installation: Uuid,
        id: PublicationId,
        handover: shorts_domain::HandoverId,
        now: DateTime<Utc>,
    ) -> DbResult<PublicationStatus> {
        let mut tx = self.pool.begin().await?;
        let held = get(&mut tx, id, true)
            .await?
            .filter(|h| h.installation == installation)
            .ok_or_else(|| DbError::not_found("publication", id))?;
        if held.handover != handover {
            return Err(DbError::Refused(
                "that is not the hand-over the runner holds".into(),
            ));
        }
        let subject = id.to_string();
        if held.publication.owner() == ExecutionOwner::Desktop {
            let job = jobs::latest_for(&mut tx, &subject).await?;
            tx.rollback().await?;
            return Ok(status(&held, job.as_ref()));
        }
        let open = jobs::open_for_update(&mut tx, &subject).await?;
        let execution = Execution {
            job_running: open
                .as_ref()
                .is_some_and(|j| j.state() == JobState::Running),
            reached_provider: reached_provider(open.as_ref()),
        };
        let mut publication = held.publication.clone();
        let transition = publication.apply(PublicationEvent::ReturnToDesktop { execution }, now)?;
        if let Some(mut job) = open.clone() {
            let loaded = job.version();
            job.cancel(now)?;
            jobs::write(&mut tx, &job, loaded).await?;
        }
        save(&mut tx, &publication, &transition).await?;
        tx.commit().await?;
        let released = HeldPublication {
            publication,
            ..held
        };
        let job = match open {
            Some(job) => self.job(job.id()).await?,
            None => self.latest_job(&subject).await?,
        };
        Ok(status(&released, job.as_ref()))
    }

    /// After the desktop sent a new credential: publications of `account` waiting for a
    /// reconnect continue (their paused job resumes, keeping its checkpoint).
    pub async fn resume_after_reconnect(
        &self,
        account: AccountId,
        now: DateTime<Utc>,
    ) -> DbResult<Vec<PublicationId>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM publications WHERE account_id = $1 AND owner = 'cloud' \
             AND state = 'needs_authentication'",
        )
        .bind(account.as_uuid())
        .fetch_all(&self.pool)
        .await?;
        let mut resumed = Vec::new();
        for raw in ids {
            let id = PublicationId::from_uuid(raw);
            let mut tx = self.pool.begin().await?;
            let Some(held) = get(&mut tx, id, true).await? else {
                continue;
            };
            let mut p = held.publication;
            let Ok(transition) = p.apply(PublicationEvent::Reauthenticated, now) else {
                continue;
            };
            save(&mut tx, &p, &transition).await?;
            let subject = id.to_string();
            match jobs::open_for_update(&mut tx, &subject).await? {
                Some(mut job) if job.state() == JobState::Paused => {
                    let loaded = job.version();
                    job.resume(now, now)?;
                    jobs::write(&mut tx, &job, loaded).await?;
                }
                Some(_) => {}
                None => {
                    jobs::insert(&mut tx, &publish_job(&p, now, PUBLISH_MAX_FAILURES, now)).await?;
                }
            }
            tx.commit().await?;
            resumed.push(id);
        }
        Ok(resumed)
    }

    /// Instagram accounts with cloud-owned publications still to publish, whose tokens the
    /// runner keeps renewed.
    pub async fn instagram_accounts_in_use(&self) -> DbResult<Vec<AccountId>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT p.account_id FROM publications p JOIN accounts a \
             ON a.id = p.account_id WHERE p.owner = 'cloud' AND p.state <> 'published' \
             AND a.platform = 'instagram_reels'",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(ids.into_iter().map(AccountId::from_uuid).collect())
    }

    pub async fn insert_attempt(&self, attempt: &PublishAttempt) -> DbResult<()> {
        sqlx::query(
            "INSERT INTO attempts (id, publication_id, record, updated_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(attempt.id.as_uuid())
        .bind(attempt.publication_id.as_uuid())
        .bind(Json(attempt))
        .bind(attempt.finished_at.unwrap_or(attempt.started_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn attempt(&self, id: AttemptId) -> DbResult<Option<PublishAttempt>> {
        let record: Option<Json<PublishAttempt>> =
            sqlx::query_scalar("SELECT record FROM attempts WHERE id = $1")
                .bind(id.as_uuid())
                .fetch_optional(&self.pool)
                .await?;
        Ok(record.map(|r| r.0))
    }

    pub async fn update_attempt(
        &self,
        attempt: &PublishAttempt,
        now: DateTime<Utc>,
    ) -> DbResult<()> {
        let done = sqlx::query("UPDATE attempts SET record = $2, updated_at = $3 WHERE id = $1")
            .bind(attempt.id.as_uuid())
            .bind(Json(attempt))
            .bind(now)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(DbError::not_found("attempt", attempt.id));
        }
        Ok(())
    }

    /// The audit log of a publication: (event, from, to), oldest first.
    pub async fn events(
        &self,
        id: PublicationId,
    ) -> DbResult<Vec<(String, Option<String>, String)>> {
        let rows = sqlx::query(
            "SELECT event, from_state, to_state FROM publication_events WHERE publication_id = $1 \
             ORDER BY id",
        )
        .bind(id.as_uuid())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("event")?,
                    r.try_get("from_state")?,
                    r.try_get("to_state")?,
                ))
            })
            .collect()
    }

    pub async fn quota_used(&self, bucket: &str, day: NaiveDate) -> DbResult<u32> {
        let units: Option<i32> =
            sqlx::query_scalar("SELECT units FROM quota_usage WHERE bucket = $1 AND day = $2")
                .bind(bucket)
                .bind(day)
                .fetch_optional(&self.pool)
                .await?;
        Ok(units.map_or(0, |u| u32::try_from(u).unwrap_or(0)))
    }

    pub async fn quota_spend(
        &self,
        bucket: &str,
        day: NaiveDate,
        units: u32,
        now: DateTime<Utc>,
    ) -> DbResult<u32> {
        let total: i32 = sqlx::query_scalar(
            "INSERT INTO quota_usage (bucket, day, units, updated_at) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (bucket, day) DO UPDATE SET units = quota_usage.units + EXCLUDED.units, \
             updated_at = EXCLUDED.updated_at RETURNING units",
        )
        .bind(bucket)
        .bind(day)
        .bind(i32::try_from(units).unwrap_or(i32::MAX))
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(u32::try_from(total).unwrap_or(0))
    }

    pub async fn quota_exhaust(
        &self,
        bucket: &str,
        day: NaiveDate,
        limit: u32,
        now: DateTime<Utc>,
    ) -> DbResult<()> {
        sqlx::query(
            "INSERT INTO quota_usage (bucket, day, units, updated_at) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (bucket, day) DO UPDATE SET \
             units = GREATEST(quota_usage.units, EXCLUDED.units), updated_at = EXCLUDED.updated_at",
        )
        .bind(bucket)
        .bind(day)
        .bind(i32::try_from(limit).unwrap_or(i32::MAX))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
