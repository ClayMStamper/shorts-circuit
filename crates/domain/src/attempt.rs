//! Publish attempts: the provider-facing history of one try at publishing a revision.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{AttemptId, JobId, PublicationId};
use crate::publication::ActiveStage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptOutcome {
    InProgress,
    Published {
        url: Option<String>,
        remote_id: Option<String>,
    },
    HandedOff {
        package_dir: String,
    },
    Failed {
        message: String,
    },
    /// The publish request may or may not have been accepted.
    Unknown {
        message: String,
    },
    /// Replaced by a newer attempt (e.g. after a new revision).
    Superseded,
}

impl AttemptOutcome {
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::InProgress)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishAttempt {
    pub id: AttemptId,
    pub publication_id: PublicationId,
    pub revision: u32,
    pub job_id: Option<JobId>,
    pub stage: ActiveStage,
    /// Adapter-typed upload session, opaque here.
    pub upload_session: Option<Value>,
    pub remote_media_id: Option<String>,
    pub remote_post_id: Option<String>,
    /// Set immediately before the publish request is sent. If the attempt is later found
    /// without an outcome, the post may exist and must be reconciled, not re-sent.
    pub publish_requested_at: Option<DateTime<Utc>>,
    pub outcome: AttemptOutcome,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl PublishAttempt {
    pub fn start(
        publication_id: PublicationId,
        revision: u32,
        job_id: Option<JobId>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: AttemptId::new(),
            publication_id,
            revision,
            job_id,
            stage: ActiveStage::Preparing,
            upload_session: None,
            remote_media_id: None,
            remote_post_id: None,
            publish_requested_at: None,
            outcome: AttemptOutcome::InProgress,
            started_at: now,
            finished_at: None,
        }
    }

    pub fn enter_stage(&mut self, stage: ActiveStage) {
        self.stage = stage;
    }

    pub fn mark_publish_requested(&mut self, now: DateTime<Utc>) {
        self.stage = ActiveStage::Publishing;
        self.publish_requested_at = Some(now);
    }

    /// A publish request went out and no outcome was recorded.
    pub fn publish_may_have_happened(&self) -> bool {
        self.publish_requested_at.is_some()
            && matches!(
                self.outcome,
                AttemptOutcome::InProgress | AttemptOutcome::Unknown { .. }
            )
    }

    pub fn finish(&mut self, outcome: AttemptOutcome, now: DateTime<Utc>) {
        if let AttemptOutcome::Published { remote_id, .. } = &outcome
            && remote_id.is_some()
        {
            self.remote_post_id.clone_from(remote_id);
        }
        self.outcome = outcome;
        self.finished_at = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_marker_flags_possible_duplicates() {
        let now = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        let mut a = PublishAttempt::start(PublicationId::new(), 1, None, now);
        assert!(!a.publish_may_have_happened());
        a.mark_publish_requested(now);
        assert!(a.publish_may_have_happened());
        a.finish(
            AttemptOutcome::Published {
                url: None,
                remote_id: Some("p1".into()),
            },
            now,
        );
        assert!(!a.publish_may_have_happened());
        assert_eq!(a.remote_post_id.as_deref(), Some("p1"));
        assert!(a.outcome.is_final());
    }
}
