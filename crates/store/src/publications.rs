use chrono::{DateTime, Utc};
use shorts_domain::{
    AccountId, AssetId, Platform, Publication, PublicationId, PublicationRecord,
    PublicationRevision, PublicationState, Schedule, TimeZoneName, Transition, VariantId,
};
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::codec::{RowExt, json, opt_json, opt_ts, to_i64, ts};
use crate::{StoreError, StoreResult};

#[derive(Debug, Clone, Default)]
pub struct PublicationFilter {
    pub asset_id: Option<AssetId>,
    pub account_id: Option<AccountId>,
    pub states: Vec<PublicationState>,
}

/// One row of a publication's audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationLogEntry {
    pub from: Option<PublicationState>,
    pub to: PublicationState,
    pub event: String,
    pub revision: u32,
    pub at: DateTime<Utc>,
}

pub struct Publications<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str = "p.id, p.asset_id, p.platform, p.state, p.current_revision, p.approval, \
                       p.schedule_at, p.schedule_tz, p.retry_at, p.user_action, p.last_error, \
                       p.published, p.version, p.created_at, p.updated_at, \
                       r.content AS revision_content, r.created_at AS revision_created_at";

const FROM: &str = "publications p JOIN publication_revisions r \
                    ON r.publication_id = p.id AND r.number = p.current_revision";

impl<'a> Publications<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Inserts a new publication and its first revision.
    pub async fn insert(&self, publication: &Publication) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        insert_publication(&mut tx, publication).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Persists the result of `Publication::apply`. The write only succeeds if the stored row
    /// is still at the version the transition started from; new revisions are appended, never
    /// rewritten.
    pub async fn save(
        &self,
        publication: &Publication,
        transition: &Transition,
    ) -> StoreResult<()> {
        let r = publication.record();
        if transition.publication_id != r.id || transition.to != r.state {
            return Err(StoreError::Corrupt(
                "transition does not belong to this publication state".into(),
            ));
        }
        let expected = r.version.checked_sub(1).ok_or_else(|| {
            StoreError::Corrupt("saved publication must come from a transition".into())
        })?;
        let mut tx = self.pool.begin().await?;
        if transition.new_revision {
            insert_revision(&mut tx, r.id, &r.revision).await?;
        }
        let result = sqlx::query(
            "UPDATE publications SET state = ?, current_revision = ?, account_id = ?, \
             variant_id = ?, approval = ?, schedule_at = ?, schedule_tz = ?, retry_at = ?, \
             user_action = ?, last_error = ?, published = ?, version = ?, updated_at = ? \
             WHERE id = ? AND version = ?",
        )
        .bind(r.state.as_str())
        .bind(i64::from(r.revision.number))
        .bind(r.revision.content.destination.account_id.to_string())
        .bind(r.revision.content.variant_id.to_string())
        .bind(opt_json(r.approval.as_ref()))
        .bind(opt_ts(r.schedule.map(|s| s.at)))
        .bind(r.schedule.map(|s| s.timezone.name()))
        .bind(opt_ts(r.retry_at))
        .bind(opt_json(r.user_action.as_ref()))
        .bind(&r.last_error)
        .bind(opt_json(r.published.as_ref()))
        .bind(to_i64(r.version))
        .bind(ts(r.updated_at))
        .bind(r.id.to_string())
        .bind(to_i64(expected))
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::conflict("publication", r.id));
        }
        log(
            &mut tx,
            r.id,
            Some(transition.from),
            transition.to,
            transition.event,
            transition.revision,
            transition.at,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get(&self, id: PublicationId) -> StoreResult<Option<Publication>> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM {FROM} WHERE p.id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn require(&self, id: PublicationId) -> StoreResult<Publication> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("publication", id))
    }

    /// Ordered by schedule (unscheduled last), then creation.
    pub async fn list(&self, filter: &PublicationFilter) -> StoreResult<Vec<Publication>> {
        let mut sql = format!("SELECT {COLUMNS} FROM {FROM} WHERE 1 = 1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(asset) = filter.asset_id {
            sql.push_str(" AND p.asset_id = ?");
            binds.push(asset.to_string());
        }
        if let Some(account) = filter.account_id {
            sql.push_str(" AND p.account_id = ?");
            binds.push(account.to_string());
        }
        if !filter.states.is_empty() {
            let marks = vec!["?"; filter.states.len()].join(", ");
            sql.push_str(&format!(" AND p.state IN ({marks})"));
            binds.extend(filter.states.iter().map(|s| s.as_str().to_owned()));
        }
        sql.push_str(" ORDER BY p.schedule_at IS NULL, p.schedule_at, p.created_at");
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

    /// Publications whose current revision uses `variant_id`.
    pub async fn using_variant(&self, variant_id: VariantId) -> StoreResult<Vec<Publication>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM {FROM} WHERE p.variant_id = ?"
        ))
        .bind(variant_id.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }

    /// Every revision, oldest first.
    pub async fn revisions(&self, id: PublicationId) -> StoreResult<Vec<PublicationRevision>> {
        let rows = sqlx::query(
            "SELECT number, content, created_at FROM publication_revisions \
             WHERE publication_id = ? ORDER BY number",
        )
        .bind(id.to_string())
        .fetch_all(self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(PublicationRevision {
                    number: row.u32("number")?,
                    content: row.json("content")?,
                    created_at: row.time("created_at")?,
                })
            })
            .collect()
    }

    /// The audit log, oldest first.
    pub async fn log(&self, id: PublicationId) -> StoreResult<Vec<PublicationLogEntry>> {
        let rows = sqlx::query(
            "SELECT from_state, to_state, event, revision, at FROM publication_events \
             WHERE publication_id = ? ORDER BY id",
        )
        .bind(id.to_string())
        .fetch_all(self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(PublicationLogEntry {
                    from: row
                        .opt_text("from_state")?
                        .as_deref()
                        .map(state)
                        .transpose()?,
                    to: state(&row.text("to_state")?)?,
                    event: row.text("event")?,
                    revision: row.u32("revision")?,
                    at: row.time("at")?,
                })
            })
            .collect()
    }
}

pub(crate) async fn insert_publication(
    tx: &mut Transaction<'_, Sqlite>,
    publication: &Publication,
) -> StoreResult<()> {
    let r = publication.record();
    sqlx::query(
        "INSERT INTO publications (id, asset_id, platform, state, current_revision,          account_id, variant_id, approval, schedule_at, schedule_tz, retry_at, user_action,          last_error, published, version, created_at, updated_at)          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(r.id.to_string())
    .bind(r.asset_id.to_string())
    .bind(r.platform.as_str())
    .bind(r.state.as_str())
    .bind(i64::from(r.revision.number))
    .bind(r.revision.content.destination.account_id.to_string())
    .bind(r.revision.content.variant_id.to_string())
    .bind(opt_json(r.approval.as_ref()))
    .bind(opt_ts(r.schedule.map(|s| s.at)))
    .bind(r.schedule.map(|s| s.timezone.name()))
    .bind(opt_ts(r.retry_at))
    .bind(opt_json(r.user_action.as_ref()))
    .bind(&r.last_error)
    .bind(opt_json(r.published.as_ref()))
    .bind(to_i64(r.version))
    .bind(ts(r.created_at))
    .bind(ts(r.updated_at))
    .execute(&mut **tx)
    .await?;
    insert_revision(tx, r.id, &r.revision).await?;
    log(
        tx,
        r.id,
        None,
        r.state,
        "created",
        r.revision.number,
        r.created_at,
    )
    .await
}

async fn insert_revision(
    tx: &mut Transaction<'_, Sqlite>,
    id: PublicationId,
    revision: &PublicationRevision,
) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO publication_revisions (publication_id, number, content, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(i64::from(revision.number))
    .bind(json(&revision.content))
    .bind(ts(revision.created_at))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn log(
    tx: &mut Transaction<'_, Sqlite>,
    id: PublicationId,
    from: Option<PublicationState>,
    to: PublicationState,
    event: &str,
    revision: u32,
    at: DateTime<Utc>,
) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO publication_events (publication_id, from_state, to_state, event, revision, at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(from.map(|s| s.as_str()))
    .bind(to.as_str())
    .bind(event)
    .bind(i64::from(revision))
    .bind(ts(at))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn state(raw: &str) -> StoreResult<PublicationState> {
    PublicationState::parse(raw)
        .ok_or_else(|| StoreError::Corrupt(format!("bad publication state {raw:?}")))
}

fn decode(row: &SqliteRow) -> StoreResult<Publication> {
    let platform = row.text("platform")?;
    let schedule = match (row.opt_time("schedule_at")?, row.opt_text("schedule_tz")?) {
        (Some(at), Some(tz)) => Some(Schedule::new(
            at,
            TimeZoneName::parse(&tz).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        )),
        (None, None) => None,
        _ => {
            return Err(StoreError::Corrupt(
                "schedule_at/schedule_tz mismatch".into(),
            ));
        }
    };
    Ok(Publication::from_record(PublicationRecord {
        id: row.parsed("id")?,
        asset_id: row.parsed("asset_id")?,
        platform: Platform::parse(&platform)
            .ok_or_else(|| StoreError::Corrupt(format!("bad platform {platform:?}")))?,
        state: state(&row.text("state")?)?,
        revision: PublicationRevision {
            number: row.u32("current_revision")?,
            content: row.json("revision_content")?,
            created_at: row.time("revision_created_at")?,
        },
        approval: row.opt_json("approval")?,
        schedule,
        retry_at: row.opt_time("retry_at")?,
        user_action: row.opt_json("user_action")?,
        last_error: row.opt_text("last_error")?,
        published: row.opt_json("published")?,
        version: row.u64("version")?,
        created_at: row.time("created_at")?,
        updated_at: row.time("updated_at")?,
    }))
}
