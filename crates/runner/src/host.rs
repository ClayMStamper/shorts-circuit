//! The runner's side of the shared publish pipeline: PostgreSQL, and the media the desktop
//! uploaded.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use shorts_adapters::youtube::QuotaLedger;
use shorts_adapters::{AdapterRegistry, ErasedAdapter};
use shorts_domain::{
    AccountId, AttemptId, ConnectedAccount, ExecutionOwner, Job, JobId, Platform, Publication,
    PublicationEvent, PublicationId, PublishAttempt, TransitionError,
};
use shorts_publish::{PreparedMedia, PublishHost};
use shorts_scheduler::{Clock, JobFailure};

use crate::db::{DbError, PgStore};
use crate::media::MediaDir;

fn db_failure(e: DbError) -> JobFailure {
    match e {
        // The publication moved on underneath the job (given back, superseded).
        DbError::Transition(
            TransitionError::NotAllowed { .. }
            | TransitionError::NotApproved
            | TransitionError::NotOwner { .. },
        ) => JobFailure::Abandon {
            message: e.to_string(),
        },
        other => JobFailure::transient(format!("database: {other}")),
    }
}

pub struct RunnerHost {
    pub(crate) store: PgStore,
    pub(crate) media: MediaDir,
    pub(crate) adapters: Arc<RwLock<AdapterRegistry>>,
    pub(crate) clock: Arc<dyn Clock>,
}

#[async_trait]
impl PublishHost for RunnerHost {
    fn executor(&self) -> ExecutionOwner {
        ExecutionOwner::Cloud
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    async fn publication(&self, id: PublicationId) -> Result<Option<Publication>, JobFailure> {
        Ok(self
            .store
            .publication(id)
            .await
            .map_err(db_failure)?
            .map(|h| h.publication))
    }

    async fn apply(
        &self,
        id: PublicationId,
        event: PublicationEvent,
    ) -> Result<Publication, JobFailure> {
        self.store
            .apply(id, move |_| event.clone(), self.clock.now())
            .await
            .map_err(db_failure)
    }

    async fn account(&self, id: AccountId) -> Result<ConnectedAccount, JobFailure> {
        self.store
            .account(id)
            .await
            .map_err(db_failure)?
            .ok_or_else(|| JobFailure::Invalid {
                message: "the runner doesn't know this account any more; reconnect it on the \
                          desktop"
                    .to_owned(),
            })
    }

    fn adapter(&self, platform: Platform) -> Option<Arc<dyn ErasedAdapter>> {
        self.adapters.read().expect("adapters lock").get(platform)
    }

    async fn prepare(&self, publication: &Publication) -> Result<PreparedMedia, JobFailure> {
        let held = self
            .store
            .publication(publication.id())
            .await
            .map_err(db_failure)?
            .ok_or_else(|| JobFailure::Abandon {
                message: "the publication no longer exists".to_owned(),
            })?;
        let path = self.media.path(held.installation, &held.media.sha256);
        if !path.is_file() {
            return Err(JobFailure::Invalid {
                message: "the video is missing on the cloud runner; take the publication back \
                          and send it again"
                    .to_owned(),
            });
        }
        Ok(PreparedMedia {
            path: path.to_string_lossy().into_owned(),
            size_bytes: held.media.size_bytes,
            properties: held.media.properties,
        })
    }

    async fn hand_off(
        &self,
        _publication: &Publication,
        _media: &PreparedMedia,
    ) -> Result<String, JobFailure> {
        Err(JobFailure::Invalid {
            message: "this account can't publish automatically, so the cloud runner can't post \
                      it; take it back to export a package"
                .to_owned(),
        })
    }

    async fn insert_attempt(&self, attempt: &PublishAttempt) -> Result<(), JobFailure> {
        self.store.insert_attempt(attempt).await.map_err(db_failure)
    }

    async fn attempt(&self, id: AttemptId) -> Result<Option<PublishAttempt>, JobFailure> {
        self.store.attempt(id).await.map_err(db_failure)
    }

    async fn update_attempt(&self, attempt: &PublishAttempt) -> Result<(), JobFailure> {
        self.store
            .update_attempt(attempt, self.clock.now())
            .await
            .map_err(db_failure)
    }

    async fn job(&self, id: JobId) -> Option<Job> {
        self.store.job(id).await.ok().flatten()
    }
}

/// YouTube quota spend recorded in PostgreSQL. The desktop keeps its own ledger, so neither
/// sees the other's spend; Google's `quotaExceeded` answer still stops both for the day.
pub struct PgQuota {
    pub(crate) store: PgStore,
    pub(crate) clock: Arc<dyn Clock>,
}

#[async_trait]
impl QuotaLedger for PgQuota {
    async fn used(&self, bucket: &str, day: NaiveDate) -> Result<u32, String> {
        self.store
            .quota_used(bucket, day)
            .await
            .map_err(|e| e.to_string())
    }

    async fn spend(&self, bucket: &str, day: NaiveDate, units: u32) -> Result<u32, String> {
        self.store
            .quota_spend(bucket, day, units, self.clock.now())
            .await
            .map_err(|e| e.to_string())
    }

    async fn exhaust(&self, bucket: &str, day: NaiveDate, limit: u32) -> Result<(), String> {
        self.store
            .quota_exhaust(bucket, day, limit, self.clock.now())
            .await
            .map_err(|e| e.to_string())
    }
}
