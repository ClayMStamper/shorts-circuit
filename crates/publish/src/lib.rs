//! The publish pipeline: takes an approved publication through prepare / upload / process /
//! publish with a platform adapter, or hands it off as an export package. Shared by the desktop
//! app and the cloud runner, which differ only in their [`PublishHost`]: where publications,
//! attempts and accounts are stored, how media is prepared, and who the executor is.
//!
//! The pipeline persists a [`PublishCheckpoint`] after every meaningful step. It always saves
//! the checkpoint before moving the publication forward, so after a crash the checkpoint is
//! never behind the publication. Before a publish request goes out, the checkpoint records that
//! it is about to; if the process dies before the answer is stored, the next run reconciles
//! with the provider instead of publishing again.
//!
//! Only the publication's execution owner runs it: the handler abandons jobs for publications
//! another executor owns, and the domain refuses to start them.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shorts_adapters::{
    AdapterError, ErasedAdapter, Processing, Reconciliation, Submission, UploadProgress,
};
use shorts_domain::{
    AccountId, ActiveStage, AttemptId, AttemptOutcome, ConnectedAccount, ExecutionOwner, Job,
    JobId, JobKind, MediaProperties, PauseReason, Platform, Publication, PublicationEvent,
    PublicationId, PublicationState, PublishAttempt, UserAction,
};
use shorts_scheduler::{Disposition, JobContext, JobFailure, JobHandler};
use tracing::warn;

/// Longest wait between processing polls, so cancellation and shutdown stay responsive.
const MAX_POLL_WAIT: Duration = Duration::from_secs(30);

/// A new publish job for `publication`'s current revision, due at `at`.
pub fn publish_job(
    publication: &Publication,
    at: DateTime<Utc>,
    max_failures: u32,
    now: DateTime<Utc>,
) -> Job {
    Job::new(
        JobKind::PublishPublication,
        publication.id().to_string(),
        json!({ "revision": publication.revision().number }),
        at,
        true,
        max_failures,
        now,
    )
}

/// The publication revision a job was created for.
pub fn job_revision(job: &Job) -> Option<u32> {
    job.payload()
        .get("revision")
        .and_then(Value::as_u64)
        .and_then(|r| u32::try_from(r).ok())
}

/// The media file an upload sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedMedia {
    pub path: String,
    pub size_bytes: u64,
    pub properties: MediaProperties,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Stage {
    #[default]
    Preparing,
    Uploading {
        session: Value,
    },
    Processing {
        media: Value,
    },
    Publishing {
        media: Value,
        /// Set (and saved) before the publish request is sent.
        requested: bool,
    },
}

impl Stage {
    fn active(&self) -> ActiveStage {
        match self {
            Self::Preparing => ActiveStage::Preparing,
            Self::Uploading { .. } => ActiveStage::Uploading,
            Self::Processing { .. } => ActiveStage::Processing,
            Self::Publishing { .. } => ActiveStage::Publishing,
        }
    }
}

/// What a publish job has done so far.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PublishCheckpoint {
    attempt: Option<AttemptId>,
    media: Option<PreparedMedia>,
    stage: Stage,
    /// The latest progress the platform reported, for the UI.
    #[serde(default)]
    progress: Option<StageProgress>,
}

impl PublishCheckpoint {
    /// Reads a publish job's checkpoint; an unreadable or missing one is the starting point.
    pub fn of(job: &Job) -> Self {
        job.checkpoint()
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default()
    }

    pub fn attempt(&self) -> Option<AttemptId> {
        self.attempt
    }

    /// The attempt sent media to the platform: an upload session was opened (or more).
    pub fn reached_provider(&self) -> bool {
        self.stage != Stage::Preparing
    }

    pub fn progress(&self) -> Option<&StageProgress> {
        self.progress.as_ref()
    }

    /// After the user confirmed a lost publish was not published: clears the
    /// publish-requested marker so the resumed job publishes again.
    pub fn clear_publish_request(&mut self) -> bool {
        match &self.stage {
            Stage::Publishing { media, .. } => {
                self.stage = Stage::Publishing {
                    media: media.clone(),
                    requested: false,
                };
                true
            }
            _ => false,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("checkpoint serializes")
    }
}

/// How far the current stage has got.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StageProgress {
    pub fraction: Option<f64>,
    pub detail: Option<String>,
}

/// The progress an open publish job last recorded.
pub fn progress(job: &Job) -> Option<StageProgress> {
    if job.kind() != JobKind::PublishPublication {
        return None;
    }
    PublishCheckpoint::of(job).progress
}

/// Maps an adapter error to what the worker should do about it.
pub fn adapter_failure(e: AdapterError) -> JobFailure {
    match e {
        AdapterError::Transient {
            message,
            retry_after,
        } => JobFailure::Transient {
            message,
            retry_after,
        },
        AdapterError::RateLimited {
            message,
            retry_after,
        } => JobFailure::RateLimited {
            message,
            retry_after,
        },
        AdapterError::Authentication { message } => JobFailure::AuthRequired { message },
        AdapterError::Invalid { issues } => JobFailure::Invalid {
            message: issues.join("; "),
        },
        // Outside of publish, a lost response only means the step should be repeated.
        AdapterError::ResponseLost { message } => JobFailure::Transient {
            message,
            retry_after: None,
        },
        AdapterError::Unsupported { message } | AdapterError::Permanent { message } => {
            JobFailure::Permanent { message }
        }
    }
}

/// Where the pipeline keeps its state and gets its media: the desktop app or the cloud runner.
///
/// Storage methods return [`JobFailure`]s so each host decides what a failure means (usually
/// transient; [`JobFailure::Abandon`] when the publication moved on underneath the job).
#[async_trait]
pub trait PublishHost: Send + Sync + 'static {
    /// Who runs this pipeline. Only publications it owns are executed.
    fn executor(&self) -> ExecutionOwner;

    fn now(&self) -> DateTime<Utc>;

    async fn publication(&self, id: PublicationId) -> Result<Option<Publication>, JobFailure>;

    /// Applies an event to the stored publication (optimistic concurrency, re-reading on
    /// conflict) and returns the result.
    async fn apply(
        &self,
        id: PublicationId,
        event: PublicationEvent,
    ) -> Result<Publication, JobFailure>;

    async fn account(&self, id: AccountId) -> Result<ConnectedAccount, JobFailure>;

    /// The adapter for `platform`, if this host can publish there automatically.
    fn adapter(&self, platform: Platform) -> Option<Arc<dyn ErasedAdapter>>;

    /// The media for the approved revision: the desktop renders (or reuses) the variant, the
    /// runner uses the file the desktop uploaded.
    async fn prepare(&self, publication: &Publication) -> Result<PreparedMedia, JobFailure>;

    /// For accounts without automatic publishing: writes an export package and returns its
    /// folder. A host that can't (the runner) returns an error.
    async fn hand_off(
        &self,
        publication: &Publication,
        media: &PreparedMedia,
    ) -> Result<String, JobFailure>;

    async fn insert_attempt(&self, attempt: &PublishAttempt) -> Result<(), JobFailure>;

    async fn attempt(&self, id: AttemptId) -> Result<Option<PublishAttempt>, JobFailure>;

    async fn update_attempt(&self, attempt: &PublishAttempt) -> Result<(), JobFailure>;

    /// The stored job, for its latest checkpoint.
    async fn job(&self, id: JobId) -> Option<Job>;

    /// The publication's progress changed (for the UI).
    fn changed(&self, _id: PublicationId) {}

    /// A publish job was settled (for the UI).
    fn settled(&self) {}
}

/// The [`JobHandler`] for [`JobKind::PublishPublication`].
pub struct PublishHandler<H: PublishHost> {
    host: Arc<H>,
}

impl<H: PublishHost> PublishHandler<H> {
    pub fn new(host: Arc<H>) -> Self {
        Self { host }
    }
}

struct Run<'a, H: PublishHost> {
    host: &'a H,
    ctx: &'a JobContext,
    id: PublicationId,
    checkpoint: PublishCheckpoint,
}

impl<H: PublishHost> Run<'_, H> {
    async fn save(&self) -> Result<(), JobFailure> {
        self.ctx.save_checkpoint(&self.checkpoint).await
    }

    async fn apply(&self, event: PublicationEvent) -> Result<Publication, JobFailure> {
        self.host.apply(self.id, event).await
    }

    /// Moves the publication to `stage` unless it is already there (after a crash the
    /// checkpoint can be one step ahead of the publication).
    async fn enter(&self, stage: ActiveStage) -> Result<(), JobFailure> {
        let current = self
            .host
            .publication(self.id)
            .await?
            .ok_or_else(|| JobFailure::Abandon {
                message: "the publication no longer exists".to_owned(),
            })?;
        if current.state() == stage.state() {
            return Ok(());
        }
        self.apply(PublicationEvent::Advance { to: stage }).await?;
        Ok(())
    }

    async fn attempt(&self) -> Result<Option<PublishAttempt>, JobFailure> {
        match self.checkpoint.attempt {
            Some(id) => self.host.attempt(id).await,
            None => Ok(None),
        }
    }

    async fn update_attempt(&self, f: impl FnOnce(&mut PublishAttempt)) -> Result<(), JobFailure> {
        if let Some(mut attempt) = self.attempt().await? {
            f(&mut attempt);
            self.host.update_attempt(&attempt).await?;
        }
        Ok(())
    }

    async fn published(
        &self,
        remote_id: String,
        url: Option<String>,
        note: Option<String>,
    ) -> Result<(), JobFailure> {
        let now = self.host.now();
        let (rid, u) = (remote_id.clone(), url.clone());
        self.update_attempt(|a| {
            a.finish(
                AttemptOutcome::Published {
                    url: u,
                    remote_id: Some(rid),
                },
                now,
            )
        })
        .await?;
        self.apply(PublicationEvent::Published {
            url,
            remote_id: Some(remote_id),
            note,
        })
        .await?;
        Ok(())
    }
}

#[async_trait]
impl<H: PublishHost> JobHandler for PublishHandler<H> {
    async fn run(&self, job: &Job, ctx: &JobContext) -> Result<(), JobFailure> {
        let host = self.host.as_ref();
        let executor = host.executor();
        let id: PublicationId = job
            .subject()
            .parse()
            .map_err(|_| JobFailure::permanent("bad publication id"))?;
        let Some(p) = host.publication(id).await? else {
            return Err(JobFailure::Abandon {
                message: "the publication no longer exists".to_owned(),
            });
        };
        if job_revision(job) != Some(p.revision().number) {
            return Err(JobFailure::Abandon {
                message: format!("superseded by revision {}", p.revision().number),
            });
        }
        if p.owner() != executor {
            return Err(JobFailure::Abandon {
                message: format!("{} runs this publication", p.owner()),
            });
        }
        let mut run = Run {
            host,
            ctx,
            id,
            checkpoint: ctx.checkpoint().unwrap_or_default(),
        };

        match p.state() {
            PublicationState::Scheduled => {
                run.apply(PublicationEvent::BeginPreparing { by: executor })
                    .await?;
            }
            PublicationState::RetryScheduled => {
                let at = run.checkpoint.stage.active();
                run.apply(PublicationEvent::Resume { at, by: executor })
                    .await?;
            }
            // Recovering an interrupted run.
            s if s.is_active() => {}
            s => {
                return Err(JobFailure::Abandon {
                    message: format!("the publication is {s}"),
                });
            }
        }

        if run.checkpoint.attempt.is_none() {
            let attempt =
                PublishAttempt::start(id, p.revision().number, Some(job.id()), host.now());
            host.insert_attempt(&attempt).await?;
            run.checkpoint.attempt = Some(attempt.id);
            run.save().await?;
        }

        let account = host.account(p.content().destination.account_id).await?;
        let adapter = if account.capabilities.can_publish_automatically() {
            host.adapter(p.platform())
        } else {
            None
        };

        if run.checkpoint.stage == Stage::Preparing {
            let prepared = host.prepare(&p).await?;
            run.checkpoint.media = Some(prepared.clone());
            run.save().await?;
            let Some(adapter) = adapter.as_ref() else {
                // Manual handoff: the package is the deliverable.
                let package_dir = host.hand_off(&p, &prepared).await?;
                let now = host.now();
                let dir = package_dir.clone();
                run.update_attempt(|a| {
                    a.finish(AttemptOutcome::HandedOff { package_dir: dir }, now)
                })
                .await?;
                run.apply(PublicationEvent::HandoffReady { package_dir })
                    .await?;
                return Ok(());
            };
            let sub = Submission {
                publication: &p,
                account: &account,
                media_path: Path::new(&prepared.path),
                media: &prepared.properties,
                size_bytes: prepared.size_bytes,
            };
            let issues = adapter.validate(&sub).await.map_err(adapter_failure)?;
            if !issues.is_empty() {
                return Err(JobFailure::Invalid {
                    message: issues
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; "),
                });
            }
            let session = adapter.begin_upload(&sub).await.map_err(adapter_failure)?;
            let recorded = session.clone();
            run.update_attempt(|a| a.upload_session = Some(recorded))
                .await?;
            run.checkpoint.stage = Stage::Uploading { session };
            run.checkpoint.progress = Some(StageProgress {
                fraction: Some(0.0),
                detail: None,
            });
            run.save().await?;
        }

        let Some(adapter) = adapter else {
            return Err(JobFailure::Invalid {
                message: format!(
                    "{} has no automatic publishing access any more; export the package instead",
                    account.display_name
                ),
            });
        };
        let prepared = run
            .checkpoint
            .media
            .clone()
            .ok_or_else(|| JobFailure::permanent("checkpoint has no prepared media"))?;
        drive_upload(&mut run, adapter.as_ref(), &p, &account, &prepared).await
    }

    async fn on_settled(&self, job: &Job, disposition: &Disposition) {
        let host = self.host.as_ref();
        host.settled();
        let Ok(id) = job.subject().parse::<PublicationId>() else {
            return;
        };
        // Only the owner records outcomes: a job for a publication another executor owns
        // was never run here.
        match host.publication(id).await {
            Ok(Some(p)) if p.owner() == host.executor() => {}
            _ => return,
        }
        let event = match disposition {
            Disposition::Succeeded | Disposition::Cancelled { .. } => return,
            Disposition::RetryAt { at, error, .. } => PublicationEvent::RetryScheduled {
                at: *at,
                reason: error.clone(),
            },
            Disposition::Paused { reason, message } => match reason {
                PauseReason::NeedsAuthentication => PublicationEvent::AuthenticationRequired {
                    message: message.clone(),
                },
                PauseReason::NeedsUserAction => PublicationEvent::UserActionRequired {
                    action: UserAction::FixValidation {
                        issues: vec![message.clone()],
                    },
                },
                PauseReason::OutcomeUnknown => PublicationEvent::OutcomeUnknown {
                    message: message.clone(),
                },
            },
            Disposition::Failed { message } => PublicationEvent::Failed {
                message: message.clone(),
            },
            Disposition::Expired { .. } => PublicationEvent::ScheduleExpired,
        };
        if let Err(e) = host.apply(id, event).await {
            warn!(publication = %id, error = %e, "could not mirror job outcome");
        }
        let outcome = match disposition {
            Disposition::Failed { message } => Some(AttemptOutcome::Failed {
                message: message.clone(),
            }),
            Disposition::Paused {
                reason: PauseReason::OutcomeUnknown,
                message,
            } => Some(AttemptOutcome::Unknown {
                message: message.clone(),
            }),
            _ => None,
        };
        if let Some(outcome) = outcome {
            // `job` is the copy claimed at the start; the stored row has the latest checkpoint.
            let checkpoint = host
                .job(job.id())
                .await
                .map(|j| PublishCheckpoint::of(&j))
                .unwrap_or_default();
            if let Some(attempt_id) = checkpoint.attempt
                && let Ok(Some(mut attempt)) = host.attempt(attempt_id).await
            {
                attempt.finish(outcome, host.now());
                let _ = host.update_attempt(&attempt).await;
            }
        }
    }
}

async fn drive_upload<H: PublishHost>(
    run: &mut Run<'_, H>,
    adapter: &dyn ErasedAdapter,
    p: &Publication,
    account: &ConnectedAccount,
    prepared: &PreparedMedia,
) -> Result<(), JobFailure> {
    let sub = Submission {
        publication: p,
        account,
        media_path: Path::new(&prepared.path),
        media: &prepared.properties,
        size_bytes: prepared.size_bytes,
    };
    loop {
        run.enter(run.checkpoint.stage.active()).await?;
        match run.checkpoint.stage.clone() {
            Stage::Preparing => unreachable!("preparing is handled before uploading"),
            Stage::Uploading { session } => {
                match adapter
                    .upload_step(&sub, session)
                    .await
                    .map_err(adapter_failure)?
                {
                    UploadProgress::InProgress {
                        checkpoint,
                        fraction,
                    } => {
                        // A provider may open a new session (e.g. after one expired).
                        if let Stage::Uploading { session } = &run.checkpoint.stage
                            && session.get("upload_url") != checkpoint.get("upload_url")
                        {
                            let recorded = checkpoint.clone();
                            run.update_attempt(|a| a.upload_session = Some(recorded))
                                .await?;
                        }
                        run.checkpoint.stage = Stage::Uploading {
                            session: checkpoint,
                        };
                        run.checkpoint.progress = Some(StageProgress {
                            fraction: Some(fraction),
                            detail: None,
                        });
                    }
                    UploadProgress::Complete { media } => {
                        let remote = media.get("id").and_then(Value::as_str).map(str::to_owned);
                        run.update_attempt(|a| {
                            a.enter_stage(ActiveStage::Processing);
                            a.remote_media_id = remote;
                        })
                        .await?;
                        run.checkpoint.stage = Stage::Processing { media };
                        run.checkpoint.progress = None;
                    }
                }
                run.save().await?;
                run.host.changed(run.id);
            }
            Stage::Processing { media } => {
                match adapter
                    .check_processing(&media)
                    .await
                    .map_err(adapter_failure)?
                {
                    Processing::Pending { poll_after, detail } => {
                        let progress = Some(StageProgress {
                            fraction: None,
                            detail,
                        });
                        if run.checkpoint.progress != progress {
                            run.checkpoint.progress = progress;
                            run.save().await?;
                            run.host.changed(run.id);
                        }
                        tokio::select! {
                            () = tokio::time::sleep(poll_after.min(MAX_POLL_WAIT)) => {}
                            () = run.ctx.cancelled() => return Err(JobFailure::LeaseLost),
                        }
                    }
                    Processing::Ready => {
                        run.checkpoint.stage = Stage::Publishing {
                            media,
                            requested: false,
                        };
                        run.checkpoint.progress = None;
                        run.save().await?;
                    }
                    Processing::Failed { message } => {
                        return Err(JobFailure::Invalid {
                            message: format!("the platform could not process the video: {message}"),
                        });
                    }
                }
            }
            Stage::Publishing {
                media,
                requested: true,
            } => {
                // A publish request went out and its answer was never recorded.
                let attempt = run
                    .attempt()
                    .await?
                    .ok_or_else(|| JobFailure::permanent("no attempt recorded"))?;
                match adapter.reconcile(&attempt, Some(&media)).await {
                    Ok(Reconciliation::Found { post }) => {
                        return run.published(post.remote_id, post.url, post.note).await;
                    }
                    Ok(Reconciliation::NotFound) => {
                        run.checkpoint.stage = Stage::Publishing {
                            media,
                            requested: false,
                        };
                        run.save().await?;
                    }
                    Ok(Reconciliation::Unavailable { reason }) => {
                        return Err(JobFailure::OutcomeUnknown {
                            message: format!(
                                "a publish request was sent but its result is unknown ({reason})"
                            ),
                        });
                    }
                    Err(e) => return Err(adapter_failure(e)),
                }
            }
            Stage::Publishing {
                media,
                requested: false,
            } => {
                run.checkpoint.stage = Stage::Publishing {
                    media: media.clone(),
                    requested: true,
                };
                run.save().await?;
                let now = run.host.now();
                run.update_attempt(|a| a.mark_publish_requested(now))
                    .await?;
                match adapter.publish(&sub, &media).await {
                    Ok(post) => return run.published(post.remote_id, post.url, post.note).await,
                    Err(AdapterError::ResponseLost { message }) => {
                        let attempt = run
                            .attempt()
                            .await?
                            .ok_or_else(|| JobFailure::permanent("no attempt recorded"))?;
                        if let Ok(Reconciliation::Found { post }) =
                            adapter.reconcile(&attempt, Some(&media)).await
                        {
                            return run.published(post.remote_id, post.url, post.note).await;
                        }
                        // Never re-sent automatically: the post may exist.
                        return Err(JobFailure::OutcomeUnknown { message });
                    }
                    Err(e) => {
                        // The provider answered with an error, so nothing was published.
                        run.checkpoint.stage = Stage::Publishing {
                            media,
                            requested: false,
                        };
                        run.save().await?;
                        return Err(adapter_failure(e));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_say_whether_the_platform_was_reached() {
        let fresh = PublishCheckpoint::default();
        assert!(!fresh.reached_provider());
        let uploading = PublishCheckpoint {
            stage: Stage::Uploading {
                session: json!({"upload_url": "https://example.com/s"}),
            },
            ..Default::default()
        };
        assert!(uploading.reached_provider());
    }

    #[test]
    fn clearing_the_publish_request_only_touches_the_publish_stage() {
        let mut publishing = PublishCheckpoint {
            stage: Stage::Publishing {
                media: json!({"id": "m"}),
                requested: true,
            },
            ..Default::default()
        };
        assert!(publishing.clear_publish_request());
        assert_eq!(
            publishing.stage,
            Stage::Publishing {
                media: json!({"id": "m"}),
                requested: false
            }
        );
        assert!(!PublishCheckpoint::default().clear_publish_request());
    }

    #[test]
    fn checkpoint_json_is_stable() {
        // Jobs saved by earlier versions of the desktop must keep loading.
        let raw = json!({
            "attempt": null,
            "media": {"path": "a.mp4", "size_bytes": 3, "properties": MediaProperties::default()},
            "stage": {"kind": "processing", "media": {"id": "v1"}},
            "progress": {"fraction": null, "detail": "processing"}
        });
        let cp: PublishCheckpoint = serde_json::from_value(raw.clone()).unwrap();
        assert!(cp.reached_provider());
        assert_eq!(cp.to_value(), raw);
    }
}
