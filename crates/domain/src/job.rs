//! Durable jobs: a unit of work with a lease, a retry time and a checkpoint.
//!
//! The job state machine lives here; `shorts-store` persists jobs with optimistic concurrency
//! on [`Job::version`] and `shorts-scheduler` drives them.

use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::ids::JobId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Render a variant into the cache.
    RenderVariant,
    /// Take an approved publication through prepare / upload / process / publish (or handoff).
    PublishPublication,
    /// Write a publishing package to a folder on request.
    ExportPackage,
}

impl JobKind {
    pub const ALL: [JobKind; 3] = [
        Self::RenderVariant,
        Self::PublishPublication,
        Self::ExportPackage,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RenderVariant => "render_variant",
            Self::PublishPublication => "publish_publication",
            Self::ExportPackage => "export_package",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    /// Waiting for the user; never picked up automatically.
    Paused,
    Succeeded,
    Failed,
    Cancelled,
    /// Overdue under the "expire" policy.
    Expired,
}

impl JobState {
    pub const ALL: [JobState; 7] = [
        Self::Queued,
        Self::Running,
        Self::Paused,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
        Self::Expired,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Queued, running or paused: the job still owns its subject.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Paused)
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    NeedsAuthentication,
    NeedsUserAction,
    OutcomeUnknown,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsAuthentication => "needs_authentication",
            Self::NeedsUserAction => "needs_user_action",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::NeedsAuthentication,
            Self::NeedsUserAction,
            Self::OutcomeUnknown,
        ]
        .into_iter()
        .find(|r| r.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub owner: String,
    pub expires_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
}

/// What to do with time-sensitive work found overdue (e.g. the desktop app was closed at the
/// scheduled posting time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverduePolicy {
    /// Run it as soon as possible, however late.
    CatchUp,
    /// Run it if it is at most `grace_secs` late; otherwise expire it and ask the user.
    Expire { grace_secs: u64 },
}

impl Default for OverduePolicy {
    /// Conservative default while the owner decides (see `docs/human-scope.md`): expire after
    /// fifteen minutes and ask.
    fn default() -> Self {
        Self::Expire {
            grace_secs: 15 * 60,
        }
    }
}

impl OverduePolicy {
    pub fn is_expired(&self, due: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        match *self {
            Self::CatchUp => false,
            Self::Expire { grace_secs } => {
                now - due > Duration::seconds(i64::try_from(grace_secs).unwrap_or(i64::MAX))
            }
        }
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum JobError {
    #[error("cannot {action} a {state} job")]
    NotAllowed {
        state: JobState,
        action: &'static str,
    },
    #[error("the job is not leased by {owner}")]
    LeaseNotHeld { owner: String },
    #[error("the job is not due yet")]
    NotDue,
}

/// Flat, storage-shaped view of a job; see [`Job::from_record`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: JobKind,
    /// The entity the job works on, e.g. a publication or variant id.
    pub subject: String,
    pub payload: Value,
    pub state: JobState,
    /// Not before this instant (UTC).
    pub run_at: DateTime<Utc>,
    /// Subject to the overdue policy (publishing is; rendering is not).
    pub time_sensitive: bool,
    /// How many times a worker has claimed it.
    pub claims: u32,
    /// Transient failures since the last success or manual resume.
    pub failures: u32,
    pub max_failures: u32,
    pub lease: Option<Lease>,
    pub checkpoint: Option<Value>,
    pub last_error: Option<String>,
    pub pause_reason: Option<PauseReason>,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    record: JobRecord,
}

impl Job {
    pub fn new(
        kind: JobKind,
        subject: impl Into<String>,
        payload: Value,
        run_at: DateTime<Utc>,
        time_sensitive: bool,
        max_failures: u32,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            record: JobRecord {
                id: JobId::new(),
                kind,
                subject: subject.into(),
                payload,
                state: JobState::Queued,
                run_at,
                time_sensitive,
                claims: 0,
                failures: 0,
                max_failures: max_failures.max(1),
                lease: None,
                checkpoint: None,
                last_error: None,
                pause_reason: None,
                version: 0,
                created_at: now,
                updated_at: now,
            },
        }
    }

    pub fn from_record(record: JobRecord) -> Self {
        Self { record }
    }

    pub fn record(&self) -> &JobRecord {
        &self.record
    }

    pub fn id(&self) -> JobId {
        self.record.id
    }

    pub fn kind(&self) -> JobKind {
        self.record.kind
    }

    pub fn subject(&self) -> &str {
        &self.record.subject
    }

    pub fn payload(&self) -> &Value {
        &self.record.payload
    }

    pub fn state(&self) -> JobState {
        self.record.state
    }

    pub fn run_at(&self) -> DateTime<Utc> {
        self.record.run_at
    }

    pub fn lease(&self) -> Option<&Lease> {
        self.record.lease.as_ref()
    }

    pub fn checkpoint(&self) -> Option<&Value> {
        self.record.checkpoint.as_ref()
    }

    pub fn failures(&self) -> u32 {
        self.record.failures
    }

    pub fn claims(&self) -> u32 {
        self.record.claims
    }

    pub fn version(&self) -> u64 {
        self.record.version
    }

    pub fn last_error(&self) -> Option<&str> {
        self.record.last_error.as_deref()
    }

    pub fn pause_reason(&self) -> Option<PauseReason> {
        self.record.pause_reason
    }

    /// Another transient failure would exceed the retry budget.
    pub fn retries_exhausted(&self) -> bool {
        self.record.failures + 1 >= self.record.max_failures
    }

    /// Due and unleased, or running under an expired lease (its worker died).
    pub fn is_claimable(&self, now: DateTime<Utc>) -> bool {
        match self.record.state {
            JobState::Queued => self.record.run_at <= now,
            JobState::Running => self
                .record
                .lease
                .as_ref()
                .is_none_or(|l| l.expires_at <= now),
            _ => false,
        }
    }

    /// Overdue past the policy's grace period. Only time-sensitive jobs expire.
    pub fn is_overdue(&self, policy: &OverduePolicy, now: DateTime<Utc>) -> bool {
        self.record.time_sensitive && policy.is_expired(self.record.run_at, now)
    }

    pub fn claim(
        &mut self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<(), JobError> {
        if !matches!(self.record.state, JobState::Queued | JobState::Running) {
            return Err(self.not_allowed("claim"));
        }
        if !self.is_claimable(now) {
            return Err(JobError::NotDue);
        }
        self.record.state = JobState::Running;
        self.record.claims += 1;
        self.record.lease = Some(Lease {
            owner: owner.to_owned(),
            expires_at: now + ttl,
            heartbeat_at: now,
        });
        self.touch(now);
        Ok(())
    }

    /// Heartbeat: extends the lease held by `owner`.
    pub fn renew(
        &mut self,
        owner: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<(), JobError> {
        self.check_lease(owner, "renew")?;
        if let Some(lease) = self.record.lease.as_mut() {
            lease.expires_at = now + ttl;
            lease.heartbeat_at = now;
        }
        self.touch(now);
        Ok(())
    }

    pub fn save_checkpoint(
        &mut self,
        owner: &str,
        checkpoint: Value,
        now: DateTime<Utc>,
    ) -> Result<(), JobError> {
        self.check_lease(owner, "checkpoint")?;
        self.record.checkpoint = Some(checkpoint);
        self.touch(now);
        Ok(())
    }

    pub fn succeed(&mut self, owner: &str, now: DateTime<Utc>) -> Result<(), JobError> {
        self.check_lease(owner, "complete")?;
        self.record.state = JobState::Succeeded;
        self.record.lease = None;
        self.record.failures = 0;
        self.record.last_error = None;
        self.touch(now);
        Ok(())
    }

    /// Re-queues the job for `at`. Transient failures count against the retry budget;
    /// rate-limit delays do not.
    pub fn schedule_retry(
        &mut self,
        owner: &str,
        at: DateTime<Utc>,
        error: &str,
        counts_as_failure: bool,
        now: DateTime<Utc>,
    ) -> Result<(), JobError> {
        self.check_lease(owner, "retry")?;
        self.record.state = JobState::Queued;
        self.record.run_at = at;
        self.record.lease = None;
        if counts_as_failure {
            self.record.failures += 1;
        }
        self.record.last_error = Some(error.to_owned());
        self.touch(now);
        Ok(())
    }

    pub fn pause(
        &mut self,
        owner: &str,
        reason: PauseReason,
        message: &str,
        now: DateTime<Utc>,
    ) -> Result<(), JobError> {
        self.check_lease(owner, "pause")?;
        self.record.state = JobState::Paused;
        self.record.lease = None;
        self.record.pause_reason = Some(reason);
        self.record.last_error = Some(message.to_owned());
        self.touch(now);
        Ok(())
    }

    pub fn fail(&mut self, owner: &str, message: &str, now: DateTime<Utc>) -> Result<(), JobError> {
        self.check_lease(owner, "fail")?;
        self.record.state = JobState::Failed;
        self.record.lease = None;
        self.record.last_error = Some(message.to_owned());
        self.touch(now);
        Ok(())
    }

    pub fn expire(&mut self, owner: &str, now: DateTime<Utc>) -> Result<(), JobError> {
        self.check_lease(owner, "expire")?;
        self.record.state = JobState::Expired;
        self.record.lease = None;
        self.record.last_error = Some(format!(
            "overdue: was due at {}",
            self.record.run_at.format("%Y-%m-%d %H:%M:%S UTC")
        ));
        self.touch(now);
        Ok(())
    }

    /// Gives the job back without counting a failure (graceful shutdown).
    pub fn release(&mut self, owner: &str, now: DateTime<Utc>) -> Result<(), JobError> {
        self.check_lease(owner, "release")?;
        self.record.state = JobState::Queued;
        self.record.lease = None;
        self.touch(now);
        Ok(())
    }

    /// Startup recovery: a job left running by a previous process of this same execution owner
    /// goes back to the queue, keeping its checkpoint so work resumes where it stopped.
    pub fn recover_interrupted(&mut self, now: DateTime<Utc>) -> Result<(), JobError> {
        if self.record.state != JobState::Running {
            return Err(self.not_allowed("recover"));
        }
        self.record.state = JobState::Queued;
        self.record.lease = None;
        self.record.last_error = Some("interrupted; resuming from checkpoint".to_owned());
        self.touch(now);
        Ok(())
    }

    /// The user dealt with whatever paused, failed or expired the job.
    pub fn resume(&mut self, run_at: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), JobError> {
        if !matches!(
            self.record.state,
            JobState::Paused | JobState::Failed | JobState::Expired
        ) {
            return Err(self.not_allowed("resume"));
        }
        self.record.state = JobState::Queued;
        self.record.run_at = run_at;
        self.record.pause_reason = None;
        self.record.failures = 0;
        self.touch(now);
        Ok(())
    }

    /// Moves a queued job to a new time.
    pub fn reschedule(
        &mut self,
        run_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), JobError> {
        if self.record.state != JobState::Queued {
            return Err(self.not_allowed("reschedule"));
        }
        self.record.run_at = run_at;
        self.touch(now);
        Ok(())
    }

    /// Replaces the checkpoint outside a lease, e.g. after the user resolved an unknown outcome.
    pub fn amend_checkpoint(
        &mut self,
        checkpoint: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<(), JobError> {
        if self.record.state == JobState::Running {
            return Err(self.not_allowed("amend the checkpoint of"));
        }
        self.record.checkpoint = checkpoint;
        self.touch(now);
        Ok(())
    }

    /// Cancelling a running job revokes its lease; the worker notices on its next heartbeat.
    pub fn cancel(&mut self, now: DateTime<Utc>) -> Result<(), JobError> {
        if matches!(self.record.state, JobState::Succeeded | JobState::Cancelled) {
            return Err(self.not_allowed("cancel"));
        }
        self.record.state = JobState::Cancelled;
        self.record.lease = None;
        self.touch(now);
        Ok(())
    }

    fn check_lease(&self, owner: &str, action: &'static str) -> Result<(), JobError> {
        if self.record.state != JobState::Running {
            return Err(self.not_allowed(action));
        }
        match &self.record.lease {
            Some(lease) if lease.owner == owner => Ok(()),
            _ => Err(JobError::LeaseNotHeld {
                owner: owner.to_owned(),
            }),
        }
    }

    fn not_allowed(&self, action: &'static str) -> JobError {
        JobError::NotAllowed {
            state: self.record.state,
            action,
        }
    }

    fn touch(&mut self, now: DateTime<Utc>) {
        self.record.version += 1;
        self.record.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-03-08T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ttl() -> Duration {
        Duration::seconds(30)
    }

    fn job() -> Job {
        Job::new(
            JobKind::PublishPublication,
            "pub-1",
            json!({}),
            t0(),
            true,
            3,
            t0(),
        )
    }

    #[test]
    fn jobs_are_claimable_only_when_due() {
        let mut j = job();
        assert_eq!(
            j.claim("w1", t0() - Duration::seconds(1), ttl()),
            Err(JobError::NotDue)
        );
        j.claim("w1", t0(), ttl()).unwrap();
        assert_eq!(j.state(), JobState::Running);
        assert_eq!(j.claims(), 1);
        assert!(!j.is_claimable(t0() + Duration::seconds(10)));
    }

    #[test]
    fn expired_leases_can_be_reclaimed() {
        let mut j = job();
        j.claim("w1", t0(), ttl()).unwrap();
        let later = t0() + ttl();
        assert!(j.is_claimable(later));
        j.claim("w2", later, ttl()).unwrap();
        assert_eq!(j.lease().unwrap().owner, "w2");
        assert_eq!(j.claims(), 2);
        // The old owner has lost it.
        assert!(matches!(
            j.renew("w1", later, ttl()),
            Err(JobError::LeaseNotHeld { .. })
        ));
    }

    #[test]
    fn heartbeats_extend_the_lease() {
        let mut j = job();
        j.claim("w1", t0(), ttl()).unwrap();
        let beat = t0() + Duration::seconds(20);
        j.renew("w1", beat, ttl()).unwrap();
        assert_eq!(j.lease().unwrap().expires_at, beat + ttl());
        assert!(!j.is_claimable(t0() + Duration::seconds(40)));
    }

    #[test]
    fn only_the_lease_holder_can_finish() {
        let mut j = job();
        j.claim("w1", t0(), ttl()).unwrap();
        assert!(j.succeed("w2", t0()).is_err());
        assert!(j.save_checkpoint("w2", json!(1), t0()).is_err());
        j.save_checkpoint("w1", json!({"step": 2}), t0()).unwrap();
        j.succeed("w1", t0()).unwrap();
        assert_eq!(j.state(), JobState::Succeeded);
        assert_eq!(j.checkpoint(), Some(&json!({"step": 2})));
    }

    #[test]
    fn retries_count_failures_but_rate_limits_do_not() {
        let mut j = job();
        j.claim("w1", t0(), ttl()).unwrap();
        let at = t0() + Duration::minutes(1);
        j.schedule_retry("w1", at, "429", false, t0()).unwrap();
        assert_eq!(j.failures(), 0);
        assert_eq!(j.run_at(), at);
        assert_eq!(j.state(), JobState::Queued);
        j.claim("w1", at, ttl()).unwrap();
        j.schedule_retry("w1", at, "503", true, at).unwrap();
        assert_eq!(j.failures(), 1);
        assert!(!j.retries_exhausted());
        j.claim("w1", at, ttl()).unwrap();
        j.schedule_retry("w1", at, "503", true, at).unwrap();
        assert!(j.retries_exhausted());
    }

    #[test]
    fn paused_jobs_wait_for_the_user() {
        let mut j = job();
        j.claim("w1", t0(), ttl()).unwrap();
        j.pause("w1", PauseReason::OutcomeUnknown, "lost response", t0())
            .unwrap();
        assert!(!j.is_claimable(t0() + Duration::days(365)));
        j.resume(t0() + Duration::hours(1), t0()).unwrap();
        assert_eq!(j.state(), JobState::Queued);
        assert_eq!(j.pause_reason(), None);
    }

    #[test]
    fn interrupted_jobs_keep_their_checkpoint() {
        let mut j = job();
        j.claim("w1", t0(), ttl()).unwrap();
        j.save_checkpoint("w1", json!({"offset": 4096}), t0())
            .unwrap();
        j.recover_interrupted(t0()).unwrap();
        assert_eq!(j.state(), JobState::Queued);
        assert_eq!(j.checkpoint(), Some(&json!({"offset": 4096})));
        assert!(j.recover_interrupted(t0()).is_err());
    }

    #[test]
    fn cancelling_revokes_the_lease() {
        let mut j = job();
        j.claim("w1", t0(), ttl()).unwrap();
        j.cancel(t0()).unwrap();
        assert!(j.renew("w1", t0(), ttl()).is_err());
        assert!(j.cancel(t0()).is_err());
    }

    #[test]
    fn every_change_bumps_the_version() {
        let mut j = job();
        let v0 = j.version();
        j.claim("w1", t0(), ttl()).unwrap();
        j.renew("w1", t0(), ttl()).unwrap();
        j.release("w1", t0()).unwrap();
        assert_eq!(j.version(), v0 + 3);
        assert_eq!(j.failures(), 0);
    }

    #[test]
    fn overdue_policy() {
        let mut j = job();
        let policy = OverduePolicy::Expire { grace_secs: 600 };
        assert!(!j.is_overdue(&policy, t0() + Duration::seconds(600)));
        assert!(j.is_overdue(&policy, t0() + Duration::seconds(601)));
        assert!(!j.is_overdue(&OverduePolicy::CatchUp, t0() + Duration::days(30)));
        assert_eq!(
            OverduePolicy::default(),
            OverduePolicy::Expire { grace_secs: 900 }
        );
        j.claim("w1", t0(), ttl()).unwrap();
        j.expire("w1", t0()).unwrap();
        assert_eq!(j.state(), JobState::Expired);

        let render = Job::new(JobKind::RenderVariant, "v", json!({}), t0(), false, 3, t0());
        assert!(!render.is_overdue(&policy, t0() + Duration::days(1)));
    }

    #[test]
    fn kinds_and_states_round_trip() {
        for k in JobKind::ALL {
            assert_eq!(JobKind::parse(k.as_str()), Some(k));
        }
        for s in JobState::ALL {
            assert_eq!(JobState::parse(s.as_str()), Some(s));
        }
    }
}
