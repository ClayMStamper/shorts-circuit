//! Execution ownership: which side (the desktop app or the cloud runner) executes a
//! publication. Exactly one does at a time, so the two never both publish it.
//!
//! Ownership moves only through [`PublicationEvent::HandToCloud`] and
//! [`PublicationEvent::ReturnToDesktop`](crate::PublicationEvent::ReturnToDesktop); both carry
//! an [`Execution`] describing the current owner's job, read inside the same transaction that
//! saves the change. While the cloud owns a publication the desktop only mirrors the runner's
//! [`OwnerReport`]s.
//!
//! [`PublicationEvent::HandToCloud`]: crate::PublicationEvent::HandToCloud

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{HandoverId, PublicationId};
use crate::publication::{PublicationState, PublishedRef, UserAction};

/// Who executes a publication.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOwner {
    /// The desktop app's worker (the default; cloud mode is optional).
    #[default]
    Desktop,
    /// The cloud runner.
    Cloud,
}

impl ExecutionOwner {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Cloud => "cloud",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Desktop, Self::Cloud]
            .into_iter()
            .find(|o| o.as_str() == s)
    }
}

impl fmt::Display for ExecutionOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Desktop => "this computer",
            Self::Cloud => "the cloud runner",
        })
    }
}

/// The current owner's execution of a publication, as its job shows it. Read in the same
/// transaction that records an ownership change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Execution {
    /// A worker holds (or held, until its lease is reclaimed) the publish job.
    pub job_running: bool,
    /// The current attempt sent media to the platform (an upload session or uploaded media
    /// exists), so starting over elsewhere could upload it twice.
    pub reached_provider: bool,
}

impl Execution {
    /// No job is running and nothing reached the platform.
    pub const IDLE: Self = Self {
        job_running: false,
        reached_provider: false,
    };
}

/// Why an ownership change was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InFlight {
    /// A worker is running the publish job.
    JobRunning,
    /// The publication is preparing, uploading, processing or publishing.
    Active,
    /// The attempt already sent media to the platform and will continue where it is.
    ReachedProvider,
}

impl fmt::Display for InFlight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::JobRunning => "it is being worked on right now",
            Self::Active => "it is uploading or publishing",
            Self::ReachedProvider => {
                "part of the video already reached the platform; it will continue where it is"
            }
        })
    }
}

/// Checks whether the cloud runner may give a publication back. Taking a publication back is
/// refused while the cloud attempt is in flight: its job is running, it is active, or it will
/// resume an upload the platform already has (`RetryScheduled`, or `NeedsAuthentication` until
/// the account is reconnected).
pub fn check_return(state: PublicationState, execution: Execution) -> Result<(), InFlight> {
    if execution.job_running {
        return Err(InFlight::JobRunning);
    }
    if state.is_active() {
        return Err(InFlight::Active);
    }
    if execution.reached_provider
        && matches!(
            state,
            PublicationState::RetryScheduled | PublicationState::NeedsAuthentication
        )
    {
        return Err(InFlight::ReachedProvider);
    }
    Ok(())
}

/// Checks whether the desktop may hand a publication to the cloud: nothing of it may be
/// running or have reached the platform from this computer.
pub fn check_hand_over(execution: Execution) -> Result<(), InFlight> {
    if execution.job_running {
        Err(InFlight::JobRunning)
    } else if execution.reached_provider {
        Err(InFlight::ReachedProvider)
    } else {
        Ok(())
    }
}

/// The cloud runner's view of a publication it owns: what the desktop mirrors and shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReport {
    pub publication_id: PublicationId,
    pub handover: HandoverId,
    pub revision: u32,
    /// Who owns it on the runner's side: `Desktop` once it was given back.
    pub owner: ExecutionOwner,
    pub state: PublicationState,
    pub retry_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub user_action: Option<UserAction>,
    pub published: Option<PublishedRef>,
    pub reached_provider: bool,
    pub updated_at: DateTime<Utc>,
}

impl OwnerReport {
    /// States a runner can report: it only ever receives approved, scheduled work and never
    /// cancels it.
    pub fn state_is_reportable(state: PublicationState) -> bool {
        !matches!(
            state,
            PublicationState::Draft
                | PublicationState::AwaitingApproval
                | PublicationState::Cancelled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_round_trip() {
        for o in [ExecutionOwner::Desktop, ExecutionOwner::Cloud] {
            assert_eq!(ExecutionOwner::parse(o.as_str()), Some(o));
            assert_eq!(
                serde_json::to_string(&o).unwrap(),
                format!("\"{}\"", o.as_str())
            );
        }
        assert_eq!(ExecutionOwner::default(), ExecutionOwner::Desktop);
    }

    #[test]
    fn returning_is_refused_while_in_flight() {
        use PublicationState as S;
        let touched = Execution {
            job_running: false,
            reached_provider: true,
        };
        let running = Execution {
            job_running: true,
            reached_provider: false,
        };
        for state in S::ALL {
            assert_eq!(
                check_return(state, running),
                Err(InFlight::JobRunning),
                "{state}"
            );
            let idle = check_return(state, Execution::IDLE);
            assert_eq!(idle.is_err(), state.is_active(), "{state}");
            let after_upload = check_return(state, touched);
            let expected =
                state.is_active() || matches!(state, S::RetryScheduled | S::NeedsAuthentication);
            assert_eq!(after_upload.is_err(), expected, "{state}");
        }
        // The user must decide these, so they can come back even after an upload.
        for state in [
            S::OutcomeUnknown,
            S::NeedsUserAction,
            S::Failed,
            S::Published,
        ] {
            assert_eq!(check_return(state, touched), Ok(()));
        }
    }

    #[test]
    fn handing_over_needs_an_untouched_job() {
        assert_eq!(check_hand_over(Execution::IDLE), Ok(()));
        assert_eq!(
            check_hand_over(Execution {
                job_running: true,
                reached_provider: false
            }),
            Err(InFlight::JobRunning)
        );
        assert_eq!(
            check_hand_over(Execution {
                job_running: false,
                reached_provider: true
            }),
            Err(InFlight::ReachedProvider)
        );
    }
}
