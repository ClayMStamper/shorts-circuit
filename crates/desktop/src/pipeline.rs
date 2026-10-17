//! Job handlers: rendering variants, the publish pipeline, and on-demand export packages.
//!
//! The publish pipeline persists a [`PublishCheckpoint`] after every meaningful step. It always
//! saves the checkpoint before moving the publication forward, so after a crash the checkpoint
//! is never behind the publication. Before a publish request goes out, the checkpoint records
//! that it is about to; if the process dies before the answer is stored, the next run
//! reconciles with the provider instead of publishing again.

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shorts_adapters::{
    AdapterError, ErasedAdapter, PackageRequest, Processing, Reconciliation, Submission,
    UploadProgress,
};
use shorts_domain::{
    ActiveStage, Asset, AttemptId, AttemptOutcome, Job, JobKind, MediaFile, MediaProperties,
    OutcomeResolution, PauseReason, PermissionStatus, Publication, PublicationEvent, PublicationId,
    PublicationState, PublishAttempt, RenderedOutput, TransitionError, UserAction, Variant,
    VariantId,
};
use shorts_media::MediaError;
use shorts_scheduler::{Disposition, JobContext, JobFailure, JobHandler};
use tracing::{info, warn};

use crate::app::App;
use crate::error::AppError;
use crate::events::AppEvent;

/// Longest wait between processing polls, so cancellation and shutdown stay responsive.
const MAX_POLL_WAIT: Duration = Duration::from_secs(30);

pub(crate) fn job_revision(job: &Job) -> Option<u32> {
    job.payload()
        .get("revision")
        .and_then(Value::as_u64)
        .and_then(|r| u32::try_from(r).ok())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PreparedMedia {
    path: String,
    size_bytes: u64,
    properties: MediaProperties,
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct PublishCheckpoint {
    attempt: Option<AttemptId>,
    media: Option<PreparedMedia>,
    stage: Stage,
}

fn upgrade(app: &Weak<App>) -> Result<Arc<App>, JobFailure> {
    app.upgrade()
        .ok_or_else(|| JobFailure::transient("the app is shutting down"))
}

fn store_failure(e: impl std::fmt::Display) -> JobFailure {
    JobFailure::transient(format!("database: {e}"))
}

fn app_failure(e: AppError) -> JobFailure {
    match e {
        // The publication moved on underneath us (cancelled, revised, approval revoked).
        AppError::Transition(TransitionError::NotAllowed { .. } | TransitionError::NotApproved) => {
            JobFailure::Abandon {
                message: e.to_string(),
            }
        }
        AppError::Media(m) => media_failure(m),
        other => store_failure(other),
    }
}

fn media_failure(e: MediaError) -> JobFailure {
    match &e {
        _ if e.is_transient() => JobFailure::transient(e.to_string()),
        MediaError::ToolMissing { .. }
        | MediaError::MissingInput(_)
        | MediaError::InvalidRecipe(_) => JobFailure::Invalid {
            message: e.to_string(),
        },
        _ => JobFailure::permanent(e.to_string()),
    }
}

fn adapter_failure(e: AdapterError) -> JobFailure {
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

/// Notes for the person posting: permissions that are not on record.
fn permission_warnings(asset: &Asset, variant: &Variant, media: &MediaFile) -> Vec<String> {
    let mut out = Vec::new();
    if variant.recipe().produces_audio(&media.properties)
        && asset.audio_permission.status != PermissionStatus::Granted
    {
        out.push(
            "Audio permission is not recorded as granted. Sounds licensed for one platform often \
             can't be reused; mute or replace the audio if unsure."
                .to_owned(),
        );
    }
    if asset.video_permission.status != PermissionStatus::Granted {
        out.push("Video permission is not recorded as granted.".to_owned());
    }
    out
}

/// Renders (or reuses) the variant a revision was approved with.
async fn prepare_media(
    app: &App,
    publication: &Publication,
) -> Result<(PreparedMedia, Variant, MediaFile), JobFailure> {
    let content = publication.content();
    let variant = app
        .store
        .variants()
        .require(content.variant_id)
        .await
        .map_err(store_failure)?;
    if variant.revision() != content.variant_revision {
        return Err(JobFailure::Invalid {
            message: "the variant was edited after this revision was made; edit and re-approve"
                .to_owned(),
        });
    }
    let media = app
        .store
        .media_files()
        .require(variant.media_file_id)
        .await
        .map_err(store_failure)?;
    let render = render_variant(app, variant.clone(), &media).await?;
    Ok((
        PreparedMedia {
            path: render.path.clone(),
            size_bytes: render.size_bytes,
            properties: render.properties.clone(),
        },
        variant,
        media,
    ))
}

/// Returns the variant's current render, rendering it first if needed.
async fn render_variant(
    app: &App,
    mut variant: Variant,
    media: &MediaFile,
) -> Result<RenderedOutput, JobFailure> {
    if let Some(r) = variant.render()
        && Path::new(&r.path).is_file()
    {
        return Ok(r.clone());
    }
    let id = variant.id.to_string();
    let last = Mutex::new(0.0_f64);
    let progress = |fraction: f64| {
        let mut last = last.lock().expect("progress lock");
        if fraction - *last >= 0.02 || (fraction >= 1.0 && *last < 1.0) {
            *last = fraction;
            app.emit(AppEvent::RenderProgress {
                variant_id: id.clone(),
                fraction,
            });
        }
    };
    let outcome = app
        .media
        .render(media, variant.recipe(), &progress)
        .await
        .map_err(media_failure)?;
    let output = RenderedOutput {
        path: outcome.path.to_string_lossy().into_owned(),
        cache_key: outcome.cache_key,
        size_bytes: outcome.size_bytes,
        properties: outcome.properties,
        rendered_at: app.now(),
    };
    let revision = variant.revision();
    variant
        .attach_render(revision, output.clone(), app.now())
        .map_err(|e| JobFailure::Abandon {
            message: e.to_string(),
        })?;
    match app.store.variants().update(&variant, revision).await {
        Ok(()) => {}
        Err(shorts_store::StoreError::Conflict { .. }) => {
            return Err(JobFailure::Abandon {
                message: "the variant was edited while rendering".to_owned(),
            });
        }
        Err(e) => return Err(store_failure(e)),
    }
    app.emit(AppEvent::LibraryChanged);
    Ok(output)
}

// --- render ---------------------------------------------------------------------------------

pub(crate) struct RenderHandler {
    app: Weak<App>,
}

impl RenderHandler {
    pub(crate) fn new(app: Weak<App>) -> Self {
        Self { app }
    }
}

#[async_trait]
impl JobHandler for RenderHandler {
    async fn run(&self, job: &Job, _ctx: &JobContext) -> Result<(), JobFailure> {
        let app = upgrade(&self.app)?;
        let id: VariantId = job
            .subject()
            .parse()
            .map_err(|_| JobFailure::permanent("bad variant id"))?;
        let variant = app
            .store
            .variants()
            .require(id)
            .await
            .map_err(store_failure)?;
        if job_revision(job) != Some(variant.revision()) {
            return Err(JobFailure::Abandon {
                message: "the variant changed; a newer render supersedes this one".to_owned(),
            });
        }
        let media = app
            .store
            .media_files()
            .require(variant.media_file_id)
            .await
            .map_err(store_failure)?;
        let asset_id = variant.asset_id;
        render_variant(&app, variant, &media).await?;
        // Rule-created publications under an unattended grant were waiting for this render.
        if let Err(e) = app.unattended_approvals(asset_id).await {
            tracing::warn!(asset = %asset_id, error = %e, "unattended approvals failed");
        }
        Ok(())
    }

    async fn on_settled(&self, _job: &Job, _disposition: &Disposition) {
        if let Some(app) = self.app.upgrade() {
            app.emit(AppEvent::JobsChanged);
            app.emit(AppEvent::LibraryChanged);
        }
    }
}

// --- export ---------------------------------------------------------------------------------

pub(crate) struct ExportHandler {
    app: Weak<App>,
}

impl ExportHandler {
    pub(crate) fn new(app: Weak<App>) -> Self {
        Self { app }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExportResult {
    pub(crate) package_dir: String,
}

async fn write_package(
    app: &App,
    publication: &Publication,
    prepared: &PreparedMedia,
    variant: &Variant,
    media: &MediaFile,
) -> Result<String, JobFailure> {
    let account = app
        .store
        .accounts()
        .require(publication.content().destination.account_id)
        .await
        .map_err(store_failure)?;
    let asset = app
        .store
        .assets()
        .require(publication.asset_id())
        .await
        .map_err(store_failure)?;
    let warnings = permission_warnings(&asset, variant, media);
    let handoff = app.handoff().await.map_err(store_failure)?;
    let package = handoff
        .export(&PackageRequest {
            publication,
            account: &account,
            asset_title: &asset.title,
            video_path: Path::new(&prepared.path),
            media: &prepared.properties,
            size_bytes: prepared.size_bytes,
            warnings: &warnings,
            exported_at: app.now(),
        })
        .await
        .map_err(adapter_failure)?;
    Ok(package.dir.to_string_lossy().into_owned())
}

#[async_trait]
impl JobHandler for ExportHandler {
    async fn run(&self, job: &Job, ctx: &JobContext) -> Result<(), JobFailure> {
        let app = upgrade(&self.app)?;
        let id: PublicationId = job
            .subject()
            .parse()
            .map_err(|_| JobFailure::permanent("bad publication id"))?;
        let p = app
            .store
            .publications()
            .require(id)
            .await
            .map_err(store_failure)?;
        if job_revision(job) != Some(p.revision().number) {
            return Err(JobFailure::Abandon {
                message: "the publication has a newer revision".to_owned(),
            });
        }
        let (prepared, variant, media) = prepare_media(&app, &p).await?;
        let package_dir = write_package(&app, &p, &prepared, &variant, &media).await?;
        info!(publication = %id, %package_dir, "exported package");
        ctx.save_checkpoint(&ExportResult { package_dir }).await
    }

    async fn on_settled(&self, _job: &Job, _disposition: &Disposition) {
        if let Some(app) = self.app.upgrade() {
            app.emit(AppEvent::JobsChanged);
        }
    }
}

// --- publish --------------------------------------------------------------------------------

pub(crate) struct PublishHandler {
    app: Weak<App>,
}

impl PublishHandler {
    pub(crate) fn new(app: Weak<App>) -> Self {
        Self { app }
    }
}

struct Run<'a> {
    app: &'a App,
    ctx: &'a JobContext,
    id: PublicationId,
    checkpoint: PublishCheckpoint,
}

impl Run<'_> {
    async fn save(&self) -> Result<(), JobFailure> {
        self.ctx.save_checkpoint(&self.checkpoint).await
    }

    async fn apply(&self, event: PublicationEvent) -> Result<Publication, JobFailure> {
        self.app
            .apply_event(self.id, move |_| event.clone())
            .await
            .map_err(app_failure)
    }

    /// Moves the publication to `stage` unless it is already there (after a crash the
    /// checkpoint can be one step ahead of the publication).
    async fn enter(&self, stage: ActiveStage) -> Result<(), JobFailure> {
        let current = self
            .app
            .store
            .publications()
            .require(self.id)
            .await
            .map_err(store_failure)?;
        if current.state() == stage.state() {
            return Ok(());
        }
        self.apply(PublicationEvent::Advance { to: stage }).await?;
        Ok(())
    }

    async fn attempt(&self) -> Result<Option<PublishAttempt>, JobFailure> {
        match self.checkpoint.attempt {
            Some(id) => self
                .app
                .store
                .attempts()
                .get(id)
                .await
                .map_err(store_failure),
            None => Ok(None),
        }
    }

    async fn update_attempt(&self, f: impl FnOnce(&mut PublishAttempt)) -> Result<(), JobFailure> {
        if let Some(mut attempt) = self.attempt().await? {
            f(&mut attempt);
            self.app
                .store
                .attempts()
                .update(&attempt)
                .await
                .map_err(store_failure)?;
        }
        Ok(())
    }

    async fn published(&self, remote_id: String, url: Option<String>) -> Result<(), JobFailure> {
        let now = self.app.now();
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
        })
        .await?;
        Ok(())
    }
}

#[async_trait]
impl JobHandler for PublishHandler {
    async fn run(&self, job: &Job, ctx: &JobContext) -> Result<(), JobFailure> {
        let app = upgrade(&self.app)?;
        let id: PublicationId = job
            .subject()
            .parse()
            .map_err(|_| JobFailure::permanent("bad publication id"))?;
        let Some(p) = app
            .store
            .publications()
            .get(id)
            .await
            .map_err(store_failure)?
        else {
            return Err(JobFailure::Abandon {
                message: "the publication no longer exists".to_owned(),
            });
        };
        if job_revision(job) != Some(p.revision().number) {
            return Err(JobFailure::Abandon {
                message: format!("superseded by revision {}", p.revision().number),
            });
        }
        let mut run = Run {
            app: &app,
            ctx,
            id,
            checkpoint: ctx.checkpoint().unwrap_or_default(),
        };

        match p.state() {
            PublicationState::Scheduled => {
                run.apply(PublicationEvent::BeginPreparing).await?;
            }
            PublicationState::RetryScheduled => {
                let at = run.checkpoint.stage.active();
                run.apply(PublicationEvent::Resume { at }).await?;
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
            let attempt = PublishAttempt::start(id, p.revision().number, Some(job.id()), app.now());
            app.store
                .attempts()
                .insert(&attempt)
                .await
                .map_err(store_failure)?;
            run.checkpoint.attempt = Some(attempt.id);
            run.save().await?;
        }

        let account = app
            .store
            .accounts()
            .require(p.content().destination.account_id)
            .await
            .map_err(store_failure)?;
        let adapter = if account.capabilities.can_publish_automatically() {
            app.adapters.get(p.platform())
        } else {
            None
        };

        if run.checkpoint.stage == Stage::Preparing {
            let (prepared, variant, media) = prepare_media(&app, &p).await?;
            run.checkpoint.media = Some(prepared.clone());
            run.save().await?;
            let Some(adapter) = adapter.as_ref() else {
                // Manual handoff: the package is the deliverable.
                let package_dir = write_package(&app, &p, &prepared, &variant, &media).await?;
                let now = app.now();
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
            run.checkpoint.stage = Stage::Uploading { session };
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
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.emit(AppEvent::JobsChanged);
        let Ok(id) = job.subject().parse::<PublicationId>() else {
            return;
        };
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
        if let Err(e) = app.apply_event(id, move |_| event.clone()).await {
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
            let checkpoint: PublishCheckpoint = match app.store.jobs().get(job.id()).await {
                Ok(Some(j)) => j
                    .checkpoint()
                    .and_then(|c| serde_json::from_value(c.clone()).ok())
                    .unwrap_or_default(),
                _ => PublishCheckpoint::default(),
            };
            if let Some(attempt_id) = checkpoint.attempt
                && let Ok(Some(mut attempt)) = app.store.attempts().get(attempt_id).await
            {
                attempt.finish(outcome, app.now());
                let _ = app.store.attempts().update(&attempt).await;
            }
        }
    }
}

async fn drive_upload(
    run: &mut Run<'_>,
    adapter: &dyn ErasedAdapter,
    p: &Publication,
    account: &shorts_domain::ConnectedAccount,
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
                    UploadProgress::InProgress { checkpoint, .. } => {
                        run.checkpoint.stage = Stage::Uploading {
                            session: checkpoint,
                        };
                    }
                    UploadProgress::Complete { media } => {
                        let remote = media.get("id").and_then(Value::as_str).map(str::to_owned);
                        run.update_attempt(|a| {
                            a.enter_stage(ActiveStage::Processing);
                            a.remote_media_id = remote;
                        })
                        .await?;
                        run.checkpoint.stage = Stage::Processing { media };
                    }
                }
                run.save().await?;
            }
            Stage::Processing { media } => {
                match adapter
                    .check_processing(&media)
                    .await
                    .map_err(adapter_failure)?
                {
                    Processing::Pending { poll_after } => {
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
                        return run.published(post.remote_id, post.url).await;
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
                let now = run.app.now();
                run.update_attempt(|a| a.mark_publish_requested(now))
                    .await?;
                match adapter.publish(&sub, &media).await {
                    Ok(post) => return run.published(post.remote_id, post.url).await,
                    Err(AdapterError::ResponseLost { message }) => {
                        let attempt = run
                            .attempt()
                            .await?
                            .ok_or_else(|| JobFailure::permanent("no attempt recorded"))?;
                        if let Ok(Reconciliation::Found { post }) =
                            adapter.reconcile(&attempt, Some(&media)).await
                        {
                            return run.published(post.remote_id, post.url).await;
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

/// Before the user's outcome decision is applied: a "not published" answer clears the
/// publish-requested marker so the resumed job publishes again; either way the attempt records
/// the decision.
pub(crate) async fn prepare_outcome_resolution(
    app: &App,
    id: PublicationId,
    resolution: &OutcomeResolution,
) -> Result<(), AppError> {
    let Some(job) = app
        .store
        .jobs()
        .open_for(JobKind::PublishPublication, &id.to_string())
        .await?
    else {
        return Ok(());
    };
    let mut checkpoint: PublishCheckpoint = job
        .checkpoint()
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default();
    let now = app.now();
    if let Some(attempt_id) = checkpoint.attempt
        && let Some(mut attempt) = app.store.attempts().get(attempt_id).await?
    {
        attempt.finish(
            match resolution {
                OutcomeResolution::Published { url, remote_id } => AttemptOutcome::Published {
                    url: url.clone(),
                    remote_id: remote_id.clone(),
                },
                OutcomeResolution::NotPublished => AttemptOutcome::Failed {
                    message: "confirmed not published".to_owned(),
                },
            },
            now,
        );
        app.store.attempts().update(&attempt).await?;
    }
    if matches!(resolution, OutcomeResolution::NotPublished)
        && let Stage::Publishing { media, .. } = checkpoint.stage.clone()
    {
        checkpoint.stage = Stage::Publishing {
            media,
            requested: false,
        };
        let value = serde_json::to_value(&checkpoint).expect("checkpoint serializes");
        app.store
            .jobs()
            .modify(job.id(), |j| j.amend_checkpoint(Some(value.clone()), now))
            .await?;
    }
    Ok(())
}

/// The package folder recorded by a finished export job.
pub(crate) fn export_result(job: &Job) -> Option<String> {
    if job.kind() != JobKind::ExportPackage {
        return None;
    }
    job.checkpoint()
        .and_then(|c| serde_json::from_value::<ExportResult>(c.clone()).ok())
        .map(|r| r.package_dir)
}
