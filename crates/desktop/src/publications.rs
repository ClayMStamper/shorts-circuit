//! Publications: creation, editing, review, approval, scheduling and recovery actions.

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use shorts_domain::platform::full_caption;
use shorts_domain::{
    AccountId, AssetId, Destination, Job, JobKind, JobState, MediaProperties, OutcomeResolution,
    PermissionStatus, Publication, PublicationEvent, PublicationId, PublicationState,
    RevisionContent, Schedule, TimeZoneName, UserAction, ValidationIssue, VariantId,
};
use shorts_scheduler::resolve_local;
use shorts_store::{PublicationFilter, StoreError};
use tracing::warn;

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::pipeline;
use crate::views::{AccountView, Action, PublicationView, ReviewView};

const PUBLISH_MAX_FAILURES: u32 = 6;
/// Minimum spacing between two posts to the same account when picking a slot.
const SLOT_GAP_MINUTES: i64 = 30;
const SLOT_HORIZON_DAYS: u32 = 28;
/// The person approving. There is one user per desktop library for now.
pub(crate) const APPROVER: &str = "owner";

#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScheduleInput {
    /// A wall-clock time (`YYYY-MM-DDTHH:MM`) in `timezone`, or the preferred timezone.
    Local {
        local: String,
        timezone: Option<String>,
    },
    Now,
    /// The next free posting slot for the destination account.
    NextSlot,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewPublication {
    pub asset_id: AssetId,
    pub variant_id: VariantId,
    pub account_id: AccountId,
    pub caption: String,
    #[serde(default)]
    pub hashtags: Vec<String>,
    pub schedule: Option<ScheduleInput>,
}

/// Fields left as `None` are unchanged.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPublication {
    pub variant_id: Option<VariantId>,
    pub caption: Option<String>,
    pub hashtags: Option<Vec<String>>,
    pub account_id: Option<AccountId>,
}

impl App {
    pub async fn create_publication(&self, new: NewPublication) -> AppResult<PublicationView> {
        let variant = self.store.variants().require(new.variant_id).await?;
        if variant.asset_id != new.asset_id {
            return Err(AppError::Invalid(
                "the variant belongs to another asset".into(),
            ));
        }
        let account = self.store.accounts().require(new.account_id).await?;
        let content = RevisionContent::new(
            variant.id,
            variant.revision(),
            &new.caption,
            &new.hashtags,
            Destination {
                account_id: account.id,
                platform: account.platform,
            },
        )?;
        let schedule = match &new.schedule {
            Some(input) => Some(self.resolve_schedule(input, account.id, None).await?),
            None => None,
        };
        let publication = Publication::draft(new.asset_id, content, schedule, self.now());
        self.store.publications().insert(&publication).await?;
        self.emit(AppEvent::PublicationChanged {
            id: publication.id().to_string(),
        });
        self.publication_view(&publication).await
    }

    pub async fn publication(&self, id: PublicationId) -> AppResult<PublicationView> {
        let p = self.store.publications().require(id).await?;
        self.publication_view(&p).await
    }

    pub async fn list_publications(
        &self,
        asset_id: Option<AssetId>,
    ) -> AppResult<Vec<PublicationView>> {
        let mut out = Vec::new();
        for p in self
            .store
            .publications()
            .list(&PublicationFilter {
                asset_id,
                ..Default::default()
            })
            .await?
        {
            out.push(self.publication_view(&p).await?);
        }
        Ok(out)
    }

    /// Changes media, copy or destination. Creates a new revision and drops any approval.
    pub async fn edit_publication(
        &self,
        id: PublicationId,
        edit: EditPublication,
    ) -> AppResult<PublicationView> {
        let current = self.store.publications().require(id).await?;
        let mut content = current.content().clone();
        if let Some(variant_id) = edit.variant_id {
            let variant = self.store.variants().require(variant_id).await?;
            if variant.asset_id != current.asset_id() {
                return Err(AppError::Invalid(
                    "the variant belongs to another asset".into(),
                ));
            }
            content.variant_id = variant.id;
            content.variant_revision = variant.revision();
        } else {
            // Pick up recipe edits made since the last revision.
            let variant = self.store.variants().require(content.variant_id).await?;
            content.variant_revision = variant.revision();
        }
        if let Some(caption) = edit.caption {
            content.caption = caption;
        }
        if let Some(tags) = edit.hashtags {
            content.hashtags = tags;
        }
        if let Some(account_id) = edit.account_id {
            let account = self.store.accounts().require(account_id).await?;
            content.destination = Destination {
                account_id: account.id,
                platform: account.platform,
            };
        }
        let p = self
            .transition(id, move |_| PublicationEvent::Revise {
                content: content.clone(),
            })
            .await?;
        self.publication_view(&p).await
    }

    pub async fn submit_for_review(&self, id: PublicationId) -> AppResult<PublicationView> {
        let p = self
            .transition(id, |_| PublicationEvent::SubmitForReview)
            .await?;
        self.publication_view(&p).await
    }

    /// The review screen: rendered video, destination, copy and time, plus anything that
    /// should block or worry the reviewer.
    pub async fn review(&self, id: PublicationId) -> AppResult<ReviewView> {
        let p = self.store.publications().require(id).await?;
        let content = p.content();
        let account = self
            .store
            .accounts()
            .require(content.destination.account_id)
            .await?;
        let variant = self.store.variants().require(content.variant_id).await?;
        let asset = self.store.assets().require(p.asset_id()).await?;
        let media = self
            .store
            .media_files()
            .require(variant.media_file_id)
            .await?;
        let requirements = p.platform().requirements();

        let rendered = variant
            .render()
            .filter(|r| std::path::Path::new(&r.path).is_file());
        let mut issues = match rendered {
            Some(r) => requirements.check(
                &r.properties,
                r.size_bytes,
                &content.caption,
                &content.hashtags,
            ),
            None => {
                // Estimate from the recipe so copy and duration problems show before rendering.
                let enc = &variant.recipe().encoding;
                let estimate = MediaProperties {
                    duration_ms: variant
                        .recipe()
                        .output_duration_ms(media.properties.duration_ms),
                    width: enc.width,
                    height: enc.height,
                    ..Default::default()
                };
                let mut issues =
                    requirements.check(&estimate, 0, &content.caption, &content.hashtags);
                issues.insert(0, ValidationIssue::NotRendered);
                issues
            }
        };
        let stale_media = variant.revision() != content.variant_revision;
        let mut warnings = Vec::new();
        if stale_media {
            warnings.push(
                "The variant was edited after this revision was made; edit the publication to \
                 pick up the new media."
                    .to_owned(),
            );
        }
        if variant.recipe().produces_audio(&media.properties)
            && asset.audio_permission.status != PermissionStatus::Granted
        {
            issues.push(ValidationIssue::AudioPermissionMissing);
        }
        match asset.video_permission.status {
            PermissionStatus::Granted => {}
            PermissionStatus::Denied => {
                warnings.push("Video permission is recorded as denied for this asset.".to_owned())
            }
            PermissionStatus::Unknown => {
                warnings.push("No video permission is recorded for this asset.".to_owned())
            }
        }
        let can_approve = p.state() == PublicationState::AwaitingApproval
            && p.schedule().is_some()
            && rendered.is_some()
            && !stale_media;
        Ok(ReviewView {
            publication: self.publication_view(&p).await?,
            account: AccountView::from(&account),
            variant: self.variant_view(&variant).await?,
            video_path: rendered.map(|r| r.path.clone()),
            full_caption: full_caption(&content.caption, &content.hashtags),
            issue_text: issues.iter().map(ToString::to_string).collect(),
            issues,
            warnings,
            can_approve,
        })
    }

    /// Approves `revision`, which must be the current one, with its rendered media.
    pub async fn approve(&self, id: PublicationId, revision: u32) -> AppResult<PublicationView> {
        self.approve_as(id, revision, APPROVER).await
    }

    /// Approval recorded under `by` (the owner, or a rule under an unattended grant).
    pub(crate) async fn approve_as(
        &self,
        id: PublicationId,
        revision: u32,
        by: &str,
    ) -> AppResult<PublicationView> {
        let current = self.store.publications().require(id).await?;
        let variant = self
            .store
            .variants()
            .require(current.content().variant_id)
            .await?;
        if !variant
            .render()
            .is_some_and(|r| std::path::Path::new(&r.path).is_file())
        {
            return Err(AppError::Invalid(
                "render the variant and review the final video before approving".into(),
            ));
        }
        let variant_revision = variant.revision();
        let p = self
            .transition(id, move |_| PublicationEvent::Approve {
                revision,
                current_variant_revision: variant_revision,
                by: by.to_owned(),
            })
            .await?;
        self.publication_view(&p).await
    }

    pub async fn revoke_approval(&self, id: PublicationId) -> AppResult<PublicationView> {
        let p = self
            .transition(id, |_| PublicationEvent::RevokeApproval)
            .await?;
        self.publication_view(&p).await
    }

    /// Sets the posting time. Rescheduling keeps an existing approval.
    pub async fn schedule(
        &self,
        id: PublicationId,
        input: ScheduleInput,
    ) -> AppResult<PublicationView> {
        let current = self.store.publications().require(id).await?;
        let schedule = self
            .resolve_schedule(&input, current.content().destination.account_id, Some(id))
            .await?;
        let p = self
            .transition(id, move |_| PublicationEvent::Reschedule { schedule })
            .await?;
        self.publication_view(&p).await
    }

    pub async fn cancel_publication(&self, id: PublicationId) -> AppResult<PublicationView> {
        let p = self.transition(id, |_| PublicationEvent::Cancel).await?;
        self.publication_view(&p).await
    }

    pub async fn retry_publication(&self, id: PublicationId) -> AppResult<PublicationView> {
        let p = self.transition(id, |_| PublicationEvent::Retry).await?;
        self.publication_view(&p).await
    }

    pub async fn resolve_user_action(&self, id: PublicationId) -> AppResult<PublicationView> {
        let p = self
            .transition(id, |_| PublicationEvent::ResolveUserAction)
            .await?;
        self.publication_view(&p).await
    }

    /// The user posted the export package by hand.
    pub async fn confirm_posted(
        &self,
        id: PublicationId,
        url: Option<String>,
    ) -> AppResult<PublicationView> {
        let url = url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
        let p = self
            .transition(id, move |_| PublicationEvent::ConfirmManualPost {
                url: url.clone(),
            })
            .await?;
        self.publication_view(&p).await
    }

    /// The user checked the platform after a lost publish response.
    pub async fn resolve_outcome(
        &self,
        id: PublicationId,
        resolution: OutcomeResolution,
    ) -> AppResult<PublicationView> {
        pipeline::prepare_outcome_resolution(self, id, &resolution).await?;
        let p = self
            .transition(id, move |_| PublicationEvent::ResolveOutcome {
                resolution: resolution.clone(),
            })
            .await?;
        self.publication_view(&p).await
    }

    /// Writes an export package for the current revision without changing its state.
    pub async fn export_package(&self, id: PublicationId) -> AppResult<()> {
        let p = self.store.publications().require(id).await?;
        let now = self.now();
        let job = Job::new(
            JobKind::ExportPackage,
            id.to_string(),
            json!({ "revision": p.revision().number }),
            now,
            false,
            3,
            now,
        );
        match self.store.jobs().insert(&job).await {
            Ok(()) | Err(StoreError::DuplicateOpenJob { .. }) => {}
            Err(e) => return Err(e.into()),
        }
        self.poke();
        Ok(())
    }

    /// Next free posting slot for an account, from the preferred posting slots.
    pub async fn next_slot(&self, account_id: AccountId) -> AppResult<Option<DateTime<Utc>>> {
        self.free_slot(account_id, None).await
    }

    async fn free_slot(
        &self,
        account_id: AccountId,
        ignore: Option<PublicationId>,
    ) -> AppResult<Option<DateTime<Utc>>> {
        let prefs = self.preferences().await?;
        let taken: Vec<_> = self
            .store
            .publications()
            .list(&PublicationFilter {
                account_id: Some(account_id),
                ..Default::default()
            })
            .await?
            .into_iter()
            .filter(|p| Some(p.id()) != ignore && !p.state().is_terminal())
            .filter_map(|p| p.schedule().map(|s| s.at))
            .collect();
        Ok(prefs.posting_plan().next_free(
            self.now(),
            &taken,
            Duration::minutes(SLOT_GAP_MINUTES),
            SLOT_HORIZON_DAYS,
        ))
    }

    async fn resolve_schedule(
        &self,
        input: &ScheduleInput,
        account_id: AccountId,
        ignore: Option<PublicationId>,
    ) -> AppResult<Schedule> {
        let prefs = self.preferences().await?;
        match input {
            ScheduleInput::Now => Ok(Schedule::new(self.now(), prefs.timezone)),
            ScheduleInput::NextSlot => {
                let at = self.free_slot(account_id, ignore).await?.ok_or_else(|| {
                    AppError::Invalid(
                        "no free posting slot in the next four weeks; add posting slots in settings"
                            .into(),
                    )
                })?;
                Ok(Schedule::new(at, prefs.timezone))
            }
            ScheduleInput::Local { local, timezone } => {
                let tz = match timezone {
                    Some(name) => TimeZoneName::parse(name)?,
                    None => prefs.timezone,
                };
                let naive = parse_local(local)?;
                Ok(Schedule::new(resolve_local(tz, naive), tz))
            }
        }
    }

    /// Loads, applies one event (built from the current state), saves with optimistic
    /// concurrency (retrying on conflict), and brings the publish job in line.
    pub(crate) async fn transition<F>(&self, id: PublicationId, event: F) -> AppResult<Publication>
    where
        F: Fn(&Publication) -> PublicationEvent,
    {
        let p = self.apply_event(id, event).await?;
        self.sync_job(&p).await?;
        Ok(p)
    }

    /// Like [`App::transition`] but leaves jobs alone; used by the worker's own handlers.
    pub(crate) async fn apply_event<F>(&self, id: PublicationId, event: F) -> AppResult<Publication>
    where
        F: Fn(&Publication) -> PublicationEvent,
    {
        let mut last = None;
        for _ in 0..5 {
            let mut p = self.store.publications().require(id).await?;
            let transition = p.apply(event(&p), self.now())?;
            match self.store.publications().save(&p, &transition).await {
                Ok(()) => {
                    self.emit(AppEvent::PublicationChanged { id: id.to_string() });
                    return Ok(p);
                }
                Err(e @ StoreError::Conflict { .. }) => last = Some(e),
                Err(e) => return Err(e.into()),
            }
        }
        Err(last.map_or_else(|| AppError::NotFound(id.to_string()), AppError::from))
    }

    /// Makes the publish job match the publication: a due publication has a queued job at its
    /// due time; one that is no longer going out has none.
    pub(crate) async fn sync_job(&self, p: &Publication) -> AppResult<()> {
        let subject = p.id().to_string();
        let jobs = self.store.jobs();
        let now = self.now();
        let revision = p.revision().number;
        let open = jobs.open_for(JobKind::PublishPublication, &subject).await?;
        match p.due_at() {
            Some(at) => {
                if let Some(job) = open {
                    let same = pipeline::job_revision(&job) == Some(revision);
                    match job.state() {
                        JobState::Running => {}
                        JobState::Queued if same => {
                            jobs.modify(job.id(), |j| j.reschedule(at, now)).await?;
                        }
                        JobState::Paused if same => {
                            jobs.modify(job.id(), |j| j.resume(at, now)).await?;
                        }
                        _ => {
                            jobs.modify(job.id(), |j| j.cancel(now)).await?;
                            self.enqueue_publish(p, at).await?;
                        }
                    }
                } else {
                    let resumable = jobs
                        .latest_for(JobKind::PublishPublication, &subject)
                        .await?
                        .filter(|j| {
                            matches!(j.state(), JobState::Failed | JobState::Expired)
                                && pipeline::job_revision(j) == Some(revision)
                        });
                    match resumable {
                        // Keeps the checkpoint, so a retry resumes where it stopped.
                        Some(job) => {
                            jobs.modify(job.id(), |j| j.resume(at, now)).await?;
                        }
                        None => self.enqueue_publish(p, at).await?,
                    }
                }
            }
            None => {
                let withdrawn = matches!(
                    p.state(),
                    PublicationState::Draft
                        | PublicationState::AwaitingApproval
                        | PublicationState::Cancelled
                        | PublicationState::Published
                );
                if withdrawn && let Some(job) = open {
                    jobs.modify(job.id(), |j| j.cancel(now)).await?;
                }
            }
        }
        self.poke();
        Ok(())
    }

    async fn enqueue_publish(&self, p: &Publication, at: DateTime<Utc>) -> AppResult<()> {
        let job = Job::new(
            JobKind::PublishPublication,
            p.id().to_string(),
            json!({ "revision": p.revision().number }),
            at,
            true,
            PUBLISH_MAX_FAILURES,
            self.now(),
        );
        match self.store.jobs().insert(&job).await {
            Ok(()) => Ok(()),
            Err(StoreError::DuplicateOpenJob { .. }) => {
                warn!(publication = %p.id(), "publish job already open");
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) async fn publication_view(&self, p: &Publication) -> AppResult<PublicationView> {
        let content = p.content();
        let asset_title = self
            .store
            .assets()
            .get(p.asset_id())
            .await?
            .map(|a| a.title)
            .unwrap_or_default();
        let account_name = self
            .store
            .accounts()
            .get(content.destination.account_id)
            .await?
            .map(|a| a.display_name)
            .unwrap_or_default();
        let variant_name = self
            .store
            .variants()
            .get(content.variant_id)
            .await?
            .map(|v| v.name)
            .unwrap_or_default();
        let package_dir = match p.user_action() {
            Some(UserAction::PostManually { package_dir }) => Some(package_dir.clone()),
            _ => None,
        };
        let progress = if p.state().is_active() {
            self.store
                .jobs()
                .open_for(JobKind::PublishPublication, &p.id().to_string())
                .await?
                .as_ref()
                .and_then(pipeline::progress)
        } else {
            None
        };
        Ok(PublicationView {
            id: p.id().to_string(),
            asset_id: p.asset_id().to_string(),
            asset_title,
            platform: p.platform(),
            platform_name: p.platform().display_name(),
            account_id: content.destination.account_id.to_string(),
            account_name: account_name.clone(),
            state: p.state(),
            revision: p.revision().number,
            variant_id: content.variant_id.to_string(),
            variant_name,
            caption: content.caption.clone(),
            hashtags: content.hashtags.clone(),
            scheduled_at: p.schedule().map(|s| s.at),
            timezone: p.schedule().map(|s| s.timezone.name().to_owned()),
            scheduled_local: p.schedule().map(format_local),
            approved_revision: p.approval().map(|a| a.revision),
            approved: p.has_valid_approval(),
            retry_at: p.retry_at(),
            last_error: p.last_error().map(str::to_owned),
            package_dir,
            published_url: p.published().and_then(|r| r.url.clone()),
            published_note: p.published().and_then(|r| r.note.clone()),
            progress_fraction: progress.as_ref().and_then(|g| g.fraction),
            progress_text: progress.and_then(|g| g.detail),
            status_text: status_text(p, &account_name),
            actions: actions(p),
            updated_at: p.record().updated_at,
        })
    }
}

fn parse_local(raw: &str) -> AppResult<NaiveDateTime> {
    let raw = raw.trim();
    ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(raw, f).ok())
        .ok_or_else(|| {
            AppError::Invalid(format!("{raw:?} is not a date and time (YYYY-MM-DDTHH:MM)"))
        })
}

fn format_local(s: &Schedule) -> String {
    s.local().format("%a %Y-%m-%d %H:%M %Z").to_string()
}

fn status_text(p: &Publication, account: &str) -> String {
    use PublicationState as S;
    let err = p.last_error().unwrap_or("unknown error");
    match p.state() {
        S::Draft => "Draft: submit it for review when the video and copy are ready.".into(),
        S::AwaitingApproval => "Waiting for your approval.".into(),
        S::Scheduled => match p.schedule() {
            Some(s) => format!("Approved; goes out {}.", format_local(s)),
            None => "Approved.".into(),
        },
        S::Preparing => "Preparing the video and package…".into(),
        S::Uploading => "Uploading…".into(),
        S::Processing => "Waiting for the platform to process the video…".into(),
        S::Publishing => "Publishing…".into(),
        S::Published => match p.published() {
            Some(r) => match (&r.url, r.manual) {
                (Some(url), true) => format!("Posted manually: {url}"),
                (Some(url), false) => format!("Published: {url}"),
                (None, true) => "Posted manually.".into(),
                (None, false) => "Published.".into(),
            },
            None => "Published.".into(),
        },
        S::RetryScheduled => match p.retry_at() {
            Some(at) => format!(
                "Will retry at {} UTC after: {err}",
                at.format("%Y-%m-%d %H:%M")
            ),
            None => format!("Will retry after: {err}"),
        },
        S::NeedsAuthentication => format!("Reconnect {account} to continue: {err}"),
        S::NeedsUserAction => p
            .user_action()
            .map(UserAction::describe)
            .unwrap_or_else(|| "Needs your attention.".into()),
        S::OutcomeUnknown => format!(
            "The platform may or may not have published this ({err}). Check {} and record \
             what happened; it will not be retried automatically.",
            p.platform()
        ),
        S::Failed => format!("Failed: {err}. Retry it, or edit and re-approve."),
        S::Cancelled => "Cancelled.".into(),
    }
}

fn actions(p: &Publication) -> Vec<Action> {
    use Action as A;
    use PublicationState as S;
    match p.state() {
        S::Draft => vec![
            A::Edit,
            A::SubmitForReview,
            A::Reschedule,
            A::ExportPackage,
            A::Cancel,
        ],
        S::AwaitingApproval => vec![
            A::Review,
            A::Approve,
            A::Edit,
            A::Reschedule,
            A::ExportPackage,
            A::Cancel,
        ],
        S::Scheduled => vec![
            A::Review,
            A::Reschedule,
            A::PublishNow,
            A::Edit,
            A::RevokeApproval,
            A::ExportPackage,
            A::Cancel,
        ],
        S::Preparing | S::Uploading | S::Processing => vec![A::Cancel],
        S::Publishing | S::Cancelled => vec![],
        S::Published => vec![A::ExportPackage],
        S::RetryScheduled => vec![A::Edit, A::RevokeApproval, A::Cancel],
        S::NeedsAuthentication => vec![A::Reconnect, A::Edit, A::RevokeApproval, A::Cancel],
        S::NeedsUserAction => match p.user_action() {
            Some(UserAction::PostManually { .. }) => {
                vec![A::ConfirmPosted, A::ExportPackage, A::Edit, A::Cancel]
            }
            Some(UserAction::ScheduleExpired { .. }) => {
                vec![A::Reschedule, A::PublishNow, A::Edit, A::Cancel]
            }
            _ => vec![A::ResolveUserAction, A::Edit, A::Cancel],
        },
        S::OutcomeUnknown => vec![A::ResolveOutcome],
        S::Failed => vec![A::Retry, A::Edit, A::RevokeApproval, A::Cancel],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_times_parse_in_common_forms() {
        assert!(parse_local("2026-11-01T09:00").is_ok());
        assert!(parse_local("2026-11-01T09:00:30").is_ok());
        assert!(parse_local("2026-11-01 09:00").is_ok());
        assert!(parse_local("tomorrow").is_err());
    }

    #[test]
    fn schedule_input_deserializes_from_the_ui() {
        let input: ScheduleInput = serde_json::from_str(
            r#"{"kind":"local","local":"2026-11-01T09:00","timezone":"Europe/Paris"}"#,
        )
        .unwrap();
        assert!(matches!(input, ScheduleInput::Local { .. }));
        let input: ScheduleInput = serde_json::from_str(r#"{"kind":"nextSlot"}"#).unwrap();
        assert!(matches!(input, ScheduleInput::NextSlot));
    }
}
