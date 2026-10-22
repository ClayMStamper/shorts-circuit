use shorts_domain::{ActiveStage, AttemptId, PublicationId, PublishAttempt};
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;

use crate::codec::{RowExt, json, opt_json, opt_ts, parse_json, ts};
use crate::{StoreError, StoreResult};

pub struct Attempts<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str = "id, publication_id, revision, job_id, stage, upload_session, \
                       remote_media_id, remote_post_id, publish_requested_at, outcome, \
                       started_at, finished_at";

impl<'a> Attempts<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, attempt: &PublishAttempt) -> StoreResult<()> {
        sqlx::query(&format!(
            "INSERT INTO publish_attempts ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(attempt.id.to_string())
        .bind(attempt.publication_id.to_string())
        .bind(i64::from(attempt.revision))
        .bind(attempt.job_id.map(|j| j.to_string()))
        .bind(json(&attempt.stage))
        .bind(opt_json(attempt.upload_session.as_ref()))
        .bind(&attempt.remote_media_id)
        .bind(&attempt.remote_post_id)
        .bind(opt_ts(attempt.publish_requested_at))
        .bind(json(&attempt.outcome))
        .bind(ts(attempt.started_at))
        .bind(opt_ts(attempt.finished_at))
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn update(&self, attempt: &PublishAttempt) -> StoreResult<()> {
        let result = sqlx::query(
            "UPDATE publish_attempts SET stage = ?, upload_session = ?, remote_media_id = ?, \
             remote_post_id = ?, publish_requested_at = ?, outcome = ?, finished_at = ? \
             WHERE id = ?",
        )
        .bind(json(&attempt.stage))
        .bind(opt_json(attempt.upload_session.as_ref()))
        .bind(&attempt.remote_media_id)
        .bind(&attempt.remote_post_id)
        .bind(opt_ts(attempt.publish_requested_at))
        .bind(json(&attempt.outcome))
        .bind(opt_ts(attempt.finished_at))
        .bind(attempt.id.to_string())
        .execute(self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::not_found("publish attempt", attempt.id));
        }
        Ok(())
    }

    pub async fn get(&self, id: AttemptId) -> StoreResult<Option<PublishAttempt>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM publish_attempts WHERE id = ?"
        ))
        .bind(id.to_string())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    /// Oldest first.
    pub async fn for_publication(&self, id: PublicationId) -> StoreResult<Vec<PublishAttempt>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM publish_attempts WHERE publication_id = ? \
             ORDER BY started_at, id"
        ))
        .bind(id.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }
}

fn decode(row: &SqliteRow) -> StoreResult<PublishAttempt> {
    let stage: ActiveStage = parse_json(&row.text("stage")?, "stage")?;
    Ok(PublishAttempt {
        id: row.parsed("id")?,
        publication_id: row.parsed("publication_id")?,
        revision: row.u32("revision")?,
        job_id: row
            .opt_text("job_id")?
            .map(|j| {
                j.parse()
                    .map_err(|_| StoreError::Corrupt(format!("bad job id {j:?}")))
            })
            .transpose()?,
        stage,
        upload_session: row.opt_json("upload_session")?,
        remote_media_id: row.opt_text("remote_media_id")?,
        remote_post_id: row.opt_text("remote_post_id")?,
        publish_requested_at: row.opt_time("publish_requested_at")?,
        outcome: row.json("outcome")?,
        started_at: row.time("started_at")?,
        finished_at: row.opt_time("finished_at")?,
    })
}
