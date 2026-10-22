//! The job and status monitor.

use chrono::Duration;
use shorts_domain::{JobId, JobKind, JobState, PublicationState};
use shorts_store::{JobFilter, PublicationFilter};

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::pipeline;
use crate::views::{JobView, MonitorView};

/// Finished publications stay on the monitor this long.
const RECENT_DAYS: i64 = 7;

impl App {
    /// Every publication that is not finished (plus recently finished ones) and recent jobs.
    pub async fn monitor(&self) -> AppResult<MonitorView> {
        let cutoff = self.now() - Duration::days(RECENT_DAYS);
        let mut publications = Vec::new();
        for p in self
            .store
            .publications()
            .list(&PublicationFilter::default())
            .await?
        {
            let finished = matches!(
                p.state(),
                PublicationState::Published | PublicationState::Cancelled
            );
            if finished && p.record().updated_at < cutoff {
                continue;
            }
            publications.push(self.publication_view(&p).await?);
        }
        Ok(MonitorView {
            publications,
            jobs: self.list_jobs(50).await?,
        })
    }

    pub async fn list_jobs(&self, limit: u32) -> AppResult<Vec<JobView>> {
        Ok(self
            .store
            .jobs()
            .list(&JobFilter {
                limit: Some(limit),
                ..Default::default()
            })
            .await?
            .iter()
            .map(|j| JobView {
                id: j.id().to_string(),
                kind: j.kind().as_str(),
                subject: j.subject().to_owned(),
                state: j.state().as_str(),
                run_at: j.run_at(),
                claims: j.claims(),
                failures: j.failures(),
                last_error: j.last_error().map(str::to_owned),
                pause_reason: j.pause_reason().map(|r| r.as_str()),
                package_dir: pipeline::export_result(j),
                updated_at: j.record().updated_at,
            })
            .collect())
    }

    /// Re-queues a failed or paused render/export job. Publish jobs follow their publication;
    /// use the publication's actions for those.
    pub async fn retry_job(&self, id: JobId) -> AppResult<()> {
        let job = self.store.jobs().require(id).await?;
        if job.kind() == JobKind::PublishPublication {
            return Err(AppError::Invalid(
                "publish jobs are retried from the publication".into(),
            ));
        }
        let now = self.now();
        self.store.jobs().modify(id, |j| j.resume(now, now)).await?;
        self.poke();
        Ok(())
    }

    /// Cancels a render or export job.
    pub async fn cancel_job(&self, id: JobId) -> AppResult<()> {
        let job = self.store.jobs().require(id).await?;
        if job.kind() == JobKind::PublishPublication {
            return Err(AppError::Invalid(
                "cancel the publication instead of its publish job".into(),
            ));
        }
        if job.state() == JobState::Cancelled {
            return Ok(());
        }
        let now = self.now();
        self.store.jobs().modify(id, |j| j.cancel(now)).await?;
        self.poke();
        Ok(())
    }
}
