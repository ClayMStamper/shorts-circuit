//! Job handlers: rendering variants, on-demand export packages, and the desktop's host for the
//! shared publish pipeline in `shorts-publish` (which renders the variant to prepare media and
//! writes export packages for accounts without automatic publishing).

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shorts_adapters::{ErasedAdapter, PackageRequest};
use shorts_domain::{
    AccountId, Asset, AttemptId, AttemptOutcome, ConnectedAccount, ExecutionOwner, Job, JobId,
    JobKind, MediaFile, OutcomeResolution, PermissionStatus, Platform, Publication,
    PublicationEvent, PublicationId, PublishAttempt, RenderedOutput, TransitionError, Variant,
    VariantId,
};
use shorts_media::MediaError;
use shorts_publish::{
    PreparedMedia, PublishCheckpoint, PublishHost, adapter_failure, job_revision,
};
use shorts_scheduler::{Disposition, JobContext, JobFailure, JobHandler};
use tracing::info;

use crate::app::App;
use crate::error::AppError;
use crate::events::AppEvent;

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
        AppError::Transition(
            TransitionError::NotAllowed { .. }
            | TransitionError::NotApproved
            | TransitionError::NotOwner { .. }
            | TransitionError::OwnedByCloud,
        ) => JobFailure::Abandon {
            message: e.to_string(),
        },
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

/// The desktop side of the shared publish pipeline: SQLite, renders and export packages.
pub(crate) struct DesktopHost {
    app: Weak<App>,
}

impl DesktopHost {
    pub(crate) fn new(app: Weak<App>) -> Self {
        Self { app }
    }

    fn app(&self) -> Result<Arc<App>, JobFailure> {
        upgrade(&self.app)
    }
}

#[async_trait]
impl PublishHost for DesktopHost {
    fn executor(&self) -> ExecutionOwner {
        ExecutionOwner::Desktop
    }

    fn now(&self) -> DateTime<Utc> {
        self.app.upgrade().map_or_else(Utc::now, |app| app.now())
    }

    async fn publication(&self, id: PublicationId) -> Result<Option<Publication>, JobFailure> {
        self.app()?
            .store
            .publications()
            .get(id)
            .await
            .map_err(store_failure)
    }

    async fn apply(
        &self,
        id: PublicationId,
        event: PublicationEvent,
    ) -> Result<Publication, JobFailure> {
        self.app()?
            .apply_event(id, move |_| event.clone())
            .await
            .map_err(app_failure)
    }

    async fn account(&self, id: AccountId) -> Result<ConnectedAccount, JobFailure> {
        self.app()?
            .store
            .accounts()
            .require(id)
            .await
            .map_err(store_failure)
    }

    fn adapter(&self, platform: Platform) -> Option<Arc<dyn ErasedAdapter>> {
        self.app.upgrade()?.adapter(platform)
    }

    async fn prepare(&self, publication: &Publication) -> Result<PreparedMedia, JobFailure> {
        let app = self.app()?;
        Ok(prepare_media(&app, publication).await?.0)
    }

    async fn hand_off(
        &self,
        publication: &Publication,
        media: &PreparedMedia,
    ) -> Result<String, JobFailure> {
        let app = self.app()?;
        let variant = app
            .store
            .variants()
            .require(publication.content().variant_id)
            .await
            .map_err(store_failure)?;
        let original = app
            .store
            .media_files()
            .require(variant.media_file_id)
            .await
            .map_err(store_failure)?;
        write_package(&app, publication, media, &variant, &original).await
    }

    async fn insert_attempt(&self, attempt: &PublishAttempt) -> Result<(), JobFailure> {
        self.app()?
            .store
            .attempts()
            .insert(attempt)
            .await
            .map_err(store_failure)
    }

    async fn attempt(&self, id: AttemptId) -> Result<Option<PublishAttempt>, JobFailure> {
        self.app()?
            .store
            .attempts()
            .get(id)
            .await
            .map_err(store_failure)
    }

    async fn update_attempt(&self, attempt: &PublishAttempt) -> Result<(), JobFailure> {
        self.app()?
            .store
            .attempts()
            .update(attempt)
            .await
            .map_err(store_failure)
    }

    async fn job(&self, id: JobId) -> Option<Job> {
        self.app
            .upgrade()?
            .store
            .jobs()
            .get(id)
            .await
            .ok()
            .flatten()
    }

    fn changed(&self, id: PublicationId) {
        if let Some(app) = self.app.upgrade() {
            app.emit(AppEvent::PublicationChanged { id: id.to_string() });
        }
    }

    fn settled(&self) {
        if let Some(app) = self.app.upgrade() {
            app.emit(AppEvent::JobsChanged);
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
    let mut checkpoint = PublishCheckpoint::of(&job);
    let now = app.now();
    if let Some(attempt_id) = checkpoint.attempt()
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
    if matches!(resolution, OutcomeResolution::NotPublished) && checkpoint.clear_publish_request() {
        let value = checkpoint.to_value();
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
