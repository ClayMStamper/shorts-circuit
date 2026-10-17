//! Publications: one destination, an immutable revision history, an approval bound to one
//! revision, a schedule, and the publication state machine.
//!
//! ```text
//! Draft > AwaitingApproval > Scheduled > Preparing > Uploading > Processing > Publishing > Published
//! ```
//!
//! plus `RetryScheduled`, `NeedsAuthentication`, `NeedsUserAction`, `OutcomeUnknown`, `Failed`
//! and `Cancelled`. Every change goes through [`Publication::apply`].
//!
//! Each publication also has an execution owner ([`ExecutionOwner`]): only the owner starts
//! work on it, the user can't change it while the cloud runner owns it, and ownership moves
//! only through `HandToCloud` / `ReturnToDesktop` (see [`crate::ownership`]).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{AccountId, AssetId, HandoverId, PublicationId, VariantId};
use crate::ownership::{self, Execution, ExecutionOwner, InFlight, OwnerReport};
use crate::platform::Platform;
use crate::schedule::Schedule;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    Draft,
    AwaitingApproval,
    Scheduled,
    Preparing,
    Uploading,
    Processing,
    Publishing,
    Published,
    RetryScheduled,
    NeedsAuthentication,
    NeedsUserAction,
    OutcomeUnknown,
    Failed,
    Cancelled,
}

impl PublicationState {
    pub const ALL: [PublicationState; 14] = [
        Self::Draft,
        Self::AwaitingApproval,
        Self::Scheduled,
        Self::Preparing,
        Self::Uploading,
        Self::Processing,
        Self::Publishing,
        Self::Published,
        Self::RetryScheduled,
        Self::NeedsAuthentication,
        Self::NeedsUserAction,
        Self::OutcomeUnknown,
        Self::Failed,
        Self::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Scheduled => "scheduled",
            Self::Preparing => "preparing",
            Self::Uploading => "uploading",
            Self::Processing => "processing",
            Self::Publishing => "publishing",
            Self::Published => "published",
            Self::RetryScheduled => "retry_scheduled",
            Self::NeedsAuthentication => "needs_authentication",
            Self::NeedsUserAction => "needs_user_action",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == s)
    }

    /// No further transitions are possible.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Cancelled)
    }

    /// A worker is executing it.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Uploading | Self::Processing | Self::Publishing
        )
    }

    /// Copy, media and destination may be edited (creating a new revision).
    pub fn allows_revision(self) -> bool {
        matches!(
            self,
            Self::Draft
                | Self::AwaitingApproval
                | Self::Scheduled
                | Self::RetryScheduled
                | Self::NeedsAuthentication
                | Self::NeedsUserAction
                | Self::Failed
        )
    }
}

impl fmt::Display for PublicationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The worker-driven stages, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveStage {
    Preparing,
    Uploading,
    Processing,
    Publishing,
}

impl ActiveStage {
    pub fn state(self) -> PublicationState {
        match self {
            Self::Preparing => PublicationState::Preparing,
            Self::Uploading => PublicationState::Uploading,
            Self::Processing => PublicationState::Processing,
            Self::Publishing => PublicationState::Publishing,
        }
    }

    fn next(self) -> Option<Self> {
        match self {
            Self::Preparing => Some(Self::Uploading),
            Self::Uploading => Some(Self::Processing),
            Self::Processing => Some(Self::Publishing),
            Self::Publishing => None,
        }
    }

    fn of(state: PublicationState) -> Option<Self> {
        match state {
            PublicationState::Preparing => Some(Self::Preparing),
            PublicationState::Uploading => Some(Self::Uploading),
            PublicationState::Processing => Some(Self::Processing),
            PublicationState::Publishing => Some(Self::Publishing),
            _ => None,
        }
    }
}

/// Where a publication goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Destination {
    pub account_id: AccountId,
    pub platform: Platform,
}

/// The reviewable content of a publication: media, copy and destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionContent {
    pub variant_id: VariantId,
    /// The variant's recipe revision this content was reviewed against.
    pub variant_revision: u32,
    pub caption: String,
    /// Normalized: no leading `#`, no whitespace, unique, in the user's order.
    pub hashtags: Vec<String>,
    pub destination: Destination,
}

impl RevisionContent {
    /// Builds content with normalized caption and hashtags.
    pub fn new(
        variant_id: VariantId,
        variant_revision: u32,
        caption: &str,
        hashtags: &[String],
        destination: Destination,
    ) -> Result<Self, TransitionError> {
        Ok(Self {
            variant_id,
            variant_revision,
            caption: caption.trim().to_owned(),
            hashtags: normalize_hashtags(hashtags)?,
            destination,
        })
    }
}

fn normalize_hashtags(raw: &[String]) -> Result<Vec<String>, TransitionError> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let tag = tag.trim().trim_start_matches('#');
        if tag.is_empty() {
            continue;
        }
        if tag.chars().any(|c| c.is_whitespace() || c == '#') {
            return Err(TransitionError::InvalidHashtag(tag.to_owned()));
        }
        if !out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            out.push(tag.to_owned());
        }
    }
    Ok(out)
}

/// One immutable revision. Revisions are numbered from 1 and never change once created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRevision {
    pub number: u32,
    pub content: RevisionContent,
    pub created_at: DateTime<Utc>,
}

/// An approval binds to exactly one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub revision: u32,
    pub approved_at: DateTime<Utc>,
    pub approved_by: String,
    /// The schedule shown at approval time, for the audit trail.
    pub schedule_at_approval: Schedule,
}

/// Why a publication is in `NeedsUserAction`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserAction {
    /// The provider or the validator rejected the submission.
    FixValidation {
        issues: Vec<String>,
    },
    /// The app wasn't running at the scheduled time and the overdue policy is "expire".
    ScheduleExpired {
        scheduled_for: DateTime<Utc>,
    },
    /// A publishing package was exported; post it by hand and then confirm.
    PostManually {
        package_dir: String,
    },
    Other {
        message: String,
    },
}

impl UserAction {
    pub fn describe(&self) -> String {
        match self {
            Self::FixValidation { issues } => format!("Fix the submission: {}", issues.join("; ")),
            Self::ScheduleExpired { scheduled_for } => format!(
                "The posting time {} passed while the app was closed. Pick a new time or post now.",
                scheduled_for.format("%Y-%m-%d %H:%M UTC")
            ),
            Self::PostManually { package_dir } => {
                format!("Post the exported package in {package_dir}, then confirm it as published.")
            }
            Self::Other { message } => message.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedRef {
    pub url: Option<String>,
    pub remote_id: Option<String>,
    pub published_at: DateTime<Utc>,
    /// True when the user posted it through the export package.
    pub manual: bool,
    /// What the platform did that the user should know, e.g. "uploaded as private".
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeResolution {
    /// The user (or reconciliation) found the post.
    Published {
        url: Option<String>,
        remote_id: Option<String>,
    },
    /// The user confirmed it was not posted; publishing may be attempted again.
    NotPublished,
}

/// Everything that can happen to a publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublicationEvent {
    // User actions.
    SubmitForReview,
    Approve {
        revision: u32,
        /// The variant's current recipe revision, so approval of stale media is refused.
        current_variant_revision: u32,
        by: String,
    },
    RevokeApproval,
    Revise {
        content: RevisionContent,
    },
    Reschedule {
        schedule: Schedule,
    },
    /// The account was signed in again. Applied by whichever side owns the publication when
    /// it gets the new credential, so it isn't refused for cloud-owned publications.
    Reauthenticated,
    ResolveUserAction,
    ConfirmManualPost {
        url: Option<String>,
    },
    ResolveOutcome {
        resolution: OutcomeResolution,
    },
    Retry,
    Cancel,
    // Execution ownership.
    /// The desktop hands an approved, scheduled publication to the cloud runner.
    HandToCloud {
        handover: HandoverId,
        /// The desktop's publish job, read in the same transaction.
        execution: Execution,
    },
    /// The cloud runner gives the publication back (on the runner), or the desktop takes it
    /// back after the runner released it (on the desktop).
    ReturnToDesktop {
        /// The cloud runner's publish job, read in the same transaction.
        execution: Execution,
    },
    /// On the desktop: the cloud runner reported progress on a publication it owns.
    Mirror {
        report: OwnerReport,
    },
    // Worker events.
    /// `by` is the executor starting the work; it must be the owner.
    BeginPreparing {
        by: ExecutionOwner,
    },
    Advance {
        to: ActiveStage,
    },
    Resume {
        at: ActiveStage,
        by: ExecutionOwner,
    },
    HandoffReady {
        package_dir: String,
    },
    Published {
        url: Option<String>,
        remote_id: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    RetryScheduled {
        at: DateTime<Utc>,
        reason: String,
    },
    AuthenticationRequired {
        message: String,
    },
    UserActionRequired {
        action: UserAction,
    },
    ScheduleExpired,
    OutcomeUnknown {
        message: String,
    },
    Failed {
        message: String,
    },
}

impl PublicationEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SubmitForReview => "submit_for_review",
            Self::Approve { .. } => "approve",
            Self::RevokeApproval => "revoke_approval",
            Self::Revise { .. } => "revise",
            Self::Reschedule { .. } => "reschedule",
            Self::Reauthenticated => "reauthenticated",
            Self::ResolveUserAction => "resolve_user_action",
            Self::ConfirmManualPost { .. } => "confirm_manual_post",
            Self::ResolveOutcome { .. } => "resolve_outcome",
            Self::Retry => "retry",
            Self::Cancel => "cancel",
            Self::HandToCloud { .. } => "hand_to_cloud",
            Self::ReturnToDesktop { .. } => "return_to_desktop",
            Self::Mirror { .. } => "mirror",
            Self::BeginPreparing { .. } => "begin_preparing",
            Self::Advance { .. } => "advance",
            Self::Resume { .. } => "resume",
            Self::HandoffReady { .. } => "handoff_ready",
            Self::Published { .. } => "published",
            Self::RetryScheduled { .. } => "retry_scheduled",
            Self::AuthenticationRequired { .. } => "authentication_required",
            Self::UserActionRequired { .. } => "user_action_required",
            Self::ScheduleExpired => "schedule_expired",
            Self::OutcomeUnknown { .. } => "outcome_unknown",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("{event} is not allowed while the publication is {state}")]
    NotAllowed {
        state: PublicationState,
        event: &'static str,
    },
    #[error("revision {requested} is not the current revision ({current})")]
    StaleRevision { requested: u32, current: u32 },
    #[error(
        "the variant changed since this revision was made (revision {reviewed}, now {current}); revise the publication first"
    )]
    StaleMedia { reviewed: u32, current: u32 },
    #[error("the current revision is not approved")]
    NotApproved,
    #[error("a posting time is required before approval")]
    MissingSchedule,
    #[error("destination is a {found} account but this publication targets {expected}")]
    PlatformMismatch { expected: Platform, found: Platform },
    #[error("invalid hashtag {0:?}")]
    InvalidHashtag(String),
    #[error("the content is unchanged")]
    Unchanged,
    /// A user change while the cloud runner owns the publication.
    #[error("the cloud runner is publishing this; take it back to this computer first")]
    OwnedByCloud,
    #[error("{executor} cannot start this publication: {owner} owns it")]
    NotOwner {
        executor: ExecutionOwner,
        owner: ExecutionOwner,
    },
    #[error("it can't move between this computer and the cloud runner now: {0}")]
    InFlight(InFlight),
    /// A cloud report for another hand-over or revision.
    #[error("the report is for a different hand-over or revision")]
    StaleReport,
}

/// The record of one applied event, for the audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub publication_id: PublicationId,
    pub from: PublicationState,
    pub to: PublicationState,
    pub event: &'static str,
    pub revision: u32,
    /// A new revision was created by this event.
    pub new_revision: bool,
    pub approval_dropped: bool,
    pub at: DateTime<Utc>,
}

/// Flat, storage-shaped view of a publication. Only `shorts-store` should build one from
/// database rows; everything else goes through [`Publication`] methods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRecord {
    pub id: PublicationId,
    pub asset_id: AssetId,
    pub platform: Platform,
    pub state: PublicationState,
    pub revision: PublicationRevision,
    pub approval: Option<Approval>,
    pub schedule: Option<Schedule>,
    pub retry_at: Option<DateTime<Utc>>,
    pub user_action: Option<UserAction>,
    pub last_error: Option<String>,
    pub published: Option<PublishedRef>,
    /// Who executes it. Records stored before cloud mode existed are desktop-owned.
    #[serde(default)]
    pub owner: ExecutionOwner,
    /// Set while the cloud owns it: which hand-over gave it to the cloud.
    #[serde(default)]
    pub handover: Option<HandoverId>,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A publication to one destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    record: PublicationRecord,
}

impl Publication {
    /// A new draft at revision 1.
    pub fn draft(
        asset_id: AssetId,
        content: RevisionContent,
        schedule: Option<Schedule>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            record: PublicationRecord {
                id: PublicationId::new(),
                asset_id,
                platform: content.destination.platform,
                state: PublicationState::Draft,
                revision: PublicationRevision {
                    number: 1,
                    content,
                    created_at: now,
                },
                approval: None,
                schedule,
                retry_at: None,
                user_action: None,
                last_error: None,
                published: None,
                owner: ExecutionOwner::Desktop,
                handover: None,
                version: 0,
                created_at: now,
                updated_at: now,
            },
        }
    }

    /// Rebuilds a publication from storage.
    pub fn from_record(record: PublicationRecord) -> Self {
        Self { record }
    }

    pub fn record(&self) -> &PublicationRecord {
        &self.record
    }

    pub fn id(&self) -> PublicationId {
        self.record.id
    }

    pub fn asset_id(&self) -> AssetId {
        self.record.asset_id
    }

    pub fn platform(&self) -> Platform {
        self.record.platform
    }

    pub fn state(&self) -> PublicationState {
        self.record.state
    }

    pub fn revision(&self) -> &PublicationRevision {
        &self.record.revision
    }

    pub fn content(&self) -> &RevisionContent {
        &self.record.revision.content
    }

    pub fn approval(&self) -> Option<&Approval> {
        self.record.approval.as_ref()
    }

    pub fn schedule(&self) -> Option<&Schedule> {
        self.record.schedule.as_ref()
    }

    pub fn retry_at(&self) -> Option<DateTime<Utc>> {
        self.record.retry_at
    }

    pub fn user_action(&self) -> Option<&UserAction> {
        self.record.user_action.as_ref()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.record.last_error.as_deref()
    }

    pub fn published(&self) -> Option<&PublishedRef> {
        self.record.published.as_ref()
    }

    pub fn owner(&self) -> ExecutionOwner {
        self.record.owner
    }

    pub fn handover(&self) -> Option<HandoverId> {
        self.record.handover
    }

    /// The cloud runner's status in the form the desktop mirrors.
    pub fn owner_report(&self, reached_provider: bool) -> Option<OwnerReport> {
        Some(OwnerReport {
            publication_id: self.record.id,
            handover: self.record.handover?,
            revision: self.record.revision.number,
            owner: self.record.owner,
            state: self.record.state,
            retry_at: self.record.retry_at,
            last_error: self.record.last_error.clone(),
            user_action: self.record.user_action.clone(),
            published: self.record.published.clone(),
            reached_provider,
            updated_at: self.record.updated_at,
        })
    }

    /// The runner's copy of a publication the desktop handed over. Refused unless the record
    /// is cloud-owned under a hand-over, scheduled, and approved at its current revision.
    pub fn received_from_desktop(record: PublicationRecord) -> Result<Self, TransitionError> {
        let p = Self { record };
        if p.record.owner != ExecutionOwner::Cloud || p.record.handover.is_none() {
            return Err(TransitionError::NotOwner {
                executor: ExecutionOwner::Cloud,
                owner: p.record.owner,
            });
        }
        if p.record.state != PublicationState::Scheduled || p.record.schedule.is_none() {
            return Err(TransitionError::NotAllowed {
                state: p.record.state,
                event: "hand_to_cloud",
            });
        }
        if !p.has_valid_approval() {
            return Err(TransitionError::NotApproved);
        }
        Ok(p)
    }

    /// Optimistic-concurrency version; incremented by every applied event.
    pub fn version(&self) -> u64 {
        self.record.version
    }

    /// The approval exists and binds to the current revision.
    pub fn has_valid_approval(&self) -> bool {
        self.record
            .approval
            .as_ref()
            .is_some_and(|a| a.revision == self.record.revision.number)
    }

    /// When a worker should next pick this publication up, if it is waiting for one.
    pub fn due_at(&self) -> Option<DateTime<Utc>> {
        match self.record.state {
            PublicationState::Scheduled => self.record.schedule.map(|s| s.at),
            PublicationState::RetryScheduled => self.record.retry_at,
            _ => None,
        }
    }

    /// Applies an event, returning what changed. On error nothing is modified.
    pub fn apply(
        &mut self,
        event: PublicationEvent,
        now: DateTime<Utc>,
    ) -> Result<Transition, TransitionError> {
        use PublicationEvent as E;
        use PublicationState as S;

        let from = self.record.state;
        let name = event.name();
        let not_allowed = || TransitionError::NotAllowed {
            state: from,
            event: name,
        };
        let mut next = self.record.clone();
        let mut new_revision = false;
        let mut approval_dropped = false;

        let user_change = matches!(
            event,
            E::SubmitForReview
                | E::Approve { .. }
                | E::RevokeApproval
                | E::Revise { .. }
                | E::Reschedule { .. }
                | E::ResolveUserAction
                | E::ConfirmManualPost { .. }
                | E::ResolveOutcome { .. }
                | E::Retry
                | E::Cancel
        );
        if user_change && next.owner == ExecutionOwner::Cloud {
            return Err(TransitionError::OwnedByCloud);
        }

        match event {
            E::SubmitForReview => {
                if from != S::Draft {
                    return Err(not_allowed());
                }
                next.state = S::AwaitingApproval;
            }
            E::Approve {
                revision,
                current_variant_revision,
                by,
            } => {
                if from != S::AwaitingApproval {
                    return Err(not_allowed());
                }
                let current = next.revision.number;
                if revision != current {
                    return Err(TransitionError::StaleRevision {
                        requested: revision,
                        current,
                    });
                }
                let reviewed = next.revision.content.variant_revision;
                if reviewed != current_variant_revision {
                    return Err(TransitionError::StaleMedia {
                        reviewed,
                        current: current_variant_revision,
                    });
                }
                let schedule = next.schedule.ok_or(TransitionError::MissingSchedule)?;
                next.approval = Some(Approval {
                    revision,
                    approved_at: now,
                    approved_by: by,
                    schedule_at_approval: schedule,
                });
                next.state = S::Scheduled;
            }
            E::RevokeApproval => {
                if !matches!(
                    from,
                    S::Scheduled
                        | S::RetryScheduled
                        | S::NeedsAuthentication
                        | S::NeedsUserAction
                        | S::Failed
                ) || next.approval.is_none()
                {
                    return Err(not_allowed());
                }
                next.approval = None;
                approval_dropped = true;
                next.state = S::AwaitingApproval;
                next.retry_at = None;
                next.user_action = None;
            }
            E::Revise { content } => {
                if !from.allows_revision() {
                    return Err(not_allowed());
                }
                if content.destination.platform != next.platform {
                    return Err(TransitionError::PlatformMismatch {
                        expected: next.platform,
                        found: content.destination.platform,
                    });
                }
                let content = RevisionContent::new(
                    content.variant_id,
                    content.variant_revision,
                    &content.caption,
                    &content.hashtags,
                    content.destination,
                )?;
                if content == next.revision.content {
                    return Err(TransitionError::Unchanged);
                }
                next.revision = PublicationRevision {
                    number: next.revision.number + 1,
                    content,
                    created_at: now,
                };
                new_revision = true;
                approval_dropped = next.approval.take().is_some();
                next.retry_at = None;
                next.user_action = None;
                next.state = if from == S::Draft {
                    S::Draft
                } else {
                    S::AwaitingApproval
                };
            }
            E::Reschedule { schedule } => {
                let expired = matches!(
                    (from, &next.user_action),
                    (S::NeedsUserAction, Some(UserAction::ScheduleExpired { .. }))
                );
                if !matches!(from, S::Draft | S::AwaitingApproval | S::Scheduled) && !expired {
                    return Err(not_allowed());
                }
                next.schedule = Some(schedule);
                if expired {
                    next.user_action = None;
                    next.state = if self.has_valid_approval() {
                        S::Scheduled
                    } else {
                        S::AwaitingApproval
                    };
                }
            }
            E::HandToCloud {
                handover,
                execution,
            } => {
                if from != S::Scheduled || next.owner != ExecutionOwner::Desktop {
                    return Err(not_allowed());
                }
                if !self.has_valid_approval() {
                    return Err(TransitionError::NotApproved);
                }
                ownership::check_hand_over(execution).map_err(TransitionError::InFlight)?;
                next.owner = ExecutionOwner::Cloud;
                next.handover = Some(handover);
            }
            E::ReturnToDesktop { execution } => {
                if next.owner != ExecutionOwner::Cloud {
                    return Err(not_allowed());
                }
                ownership::check_return(from, execution).map_err(TransitionError::InFlight)?;
                next.owner = ExecutionOwner::Desktop;
                next.handover = None;
            }
            E::Mirror { report } => {
                if next.owner != ExecutionOwner::Cloud || from.is_terminal() {
                    return Err(not_allowed());
                }
                if Some(report.handover) != next.handover
                    || report.publication_id != next.id
                    || report.revision != next.revision.number
                {
                    return Err(TransitionError::StaleReport);
                }
                if !OwnerReport::state_is_reportable(report.state) {
                    return Err(TransitionError::NotAllowed {
                        state: report.state,
                        event: name,
                    });
                }
                let unchanged = next.state == report.state
                    && next.retry_at == report.retry_at
                    && next.last_error == report.last_error
                    && next.user_action == report.user_action
                    && next.published == report.published;
                if unchanged {
                    return Err(TransitionError::Unchanged);
                }
                next.state = report.state;
                next.retry_at = report.retry_at;
                next.last_error = report.last_error;
                next.user_action = report.user_action;
                next.published = report.published;
            }
            E::BeginPreparing { by } => {
                if from != S::Scheduled {
                    return Err(not_allowed());
                }
                if by != next.owner {
                    return Err(TransitionError::NotOwner {
                        executor: by,
                        owner: next.owner,
                    });
                }
                if !self.has_valid_approval() {
                    return Err(TransitionError::NotApproved);
                }
                next.state = S::Preparing;
                next.last_error = None;
            }
            E::Advance { to } => {
                let current = ActiveStage::of(from).ok_or_else(not_allowed)?;
                if current.next() != Some(to) {
                    return Err(not_allowed());
                }
                next.state = to.state();
            }
            E::Resume { at, by } => {
                if from != S::RetryScheduled {
                    return Err(not_allowed());
                }
                if by != next.owner {
                    return Err(TransitionError::NotOwner {
                        executor: by,
                        owner: next.owner,
                    });
                }
                if !self.has_valid_approval() {
                    return Err(TransitionError::NotApproved);
                }
                next.state = at.state();
                next.retry_at = None;
            }
            E::HandoffReady { package_dir } => {
                if from != S::Preparing {
                    return Err(not_allowed());
                }
                next.state = S::NeedsUserAction;
                next.user_action = Some(UserAction::PostManually { package_dir });
            }
            E::Published {
                url,
                remote_id,
                note,
            } => {
                if from != S::Publishing {
                    return Err(not_allowed());
                }
                next.state = S::Published;
                next.published = Some(PublishedRef {
                    url,
                    remote_id,
                    published_at: now,
                    manual: false,
                    note,
                });
                next.last_error = None;
            }
            E::RetryScheduled { at, reason } => {
                if !from.is_active() {
                    return Err(not_allowed());
                }
                next.state = S::RetryScheduled;
                next.retry_at = Some(at);
                next.last_error = Some(reason);
            }
            E::AuthenticationRequired { message } => {
                if !from.is_active() && from != S::RetryScheduled && from != S::Scheduled {
                    return Err(not_allowed());
                }
                next.state = S::NeedsAuthentication;
                next.retry_at = None;
                next.last_error = Some(message);
            }
            E::UserActionRequired { action } => {
                if !from.is_active() && from != S::RetryScheduled && from != S::Scheduled {
                    return Err(not_allowed());
                }
                next.state = S::NeedsUserAction;
                next.retry_at = None;
                next.user_action = Some(action);
            }
            E::ScheduleExpired => {
                let scheduled_for = match from {
                    S::Scheduled => next.schedule.map(|s| s.at),
                    S::RetryScheduled => next.retry_at,
                    _ => return Err(not_allowed()),
                }
                .unwrap_or(now);
                next.state = S::NeedsUserAction;
                next.retry_at = None;
                next.user_action = Some(UserAction::ScheduleExpired { scheduled_for });
            }
            E::OutcomeUnknown { message } => {
                // Only a publish request can have an unknown outcome; it is never retried
                // automatically.
                if from != S::Publishing {
                    return Err(not_allowed());
                }
                next.state = S::OutcomeUnknown;
                next.last_error = Some(message);
            }
            E::Failed { message } => {
                if !from.is_active() && from != S::RetryScheduled {
                    return Err(not_allowed());
                }
                next.state = S::Failed;
                next.retry_at = None;
                next.last_error = Some(message);
            }
            E::Reauthenticated => {
                if from != S::NeedsAuthentication {
                    return Err(not_allowed());
                }
                next.state = S::RetryScheduled;
                next.retry_at = Some(now);
            }
            E::ResolveUserAction => {
                let resolvable = matches!(
                    next.user_action,
                    Some(UserAction::FixValidation { .. } | UserAction::Other { .. })
                );
                if from != S::NeedsUserAction || !resolvable {
                    return Err(not_allowed());
                }
                if !self.has_valid_approval() {
                    return Err(TransitionError::NotApproved);
                }
                next.state = S::RetryScheduled;
                next.retry_at = Some(now);
                next.user_action = None;
            }
            E::ConfirmManualPost { url } => {
                if from != S::NeedsUserAction
                    || !matches!(next.user_action, Some(UserAction::PostManually { .. }))
                {
                    return Err(not_allowed());
                }
                next.state = S::Published;
                next.user_action = None;
                next.published = Some(PublishedRef {
                    url,
                    remote_id: None,
                    published_at: now,
                    manual: true,
                    note: None,
                });
            }
            E::ResolveOutcome { resolution } => {
                if from != S::OutcomeUnknown {
                    return Err(not_allowed());
                }
                match resolution {
                    OutcomeResolution::Published { url, remote_id } => {
                        next.state = S::Published;
                        next.published = Some(PublishedRef {
                            url,
                            remote_id,
                            published_at: now,
                            manual: false,
                            note: None,
                        });
                    }
                    OutcomeResolution::NotPublished => {
                        next.state = S::RetryScheduled;
                        next.retry_at = Some(now);
                    }
                }
                next.last_error = None;
            }
            E::Retry => {
                if from != S::Failed {
                    return Err(not_allowed());
                }
                if !self.has_valid_approval() {
                    return Err(TransitionError::NotApproved);
                }
                next.state = S::RetryScheduled;
                next.retry_at = Some(now);
            }
            E::Cancel => {
                // Publishing may already have reached the provider and OutcomeUnknown must be
                // resolved first, so neither can be cancelled.
                if from.is_terminal() || matches!(from, S::Publishing | S::OutcomeUnknown) {
                    return Err(not_allowed());
                }
                next.state = S::Cancelled;
                next.retry_at = None;
            }
        }

        next.version += 1;
        next.updated_at = now;
        let transition = Transition {
            publication_id: next.id,
            from,
            to: next.state,
            event: name,
            revision: next.revision.number,
            new_revision,
            approval_dropped,
            at: now,
        };
        self.record = next;
        Ok(transition)
    }
}

#[cfg(test)]
mod tests;
