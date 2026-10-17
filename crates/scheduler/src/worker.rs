//! The durable worker loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::SeedableRng;
use rand::rngs::StdRng;
use shorts_domain::{Job, JobKind, OverduePolicy, PauseReason};
use shorts_store::{Store, StoreError, StoreResult};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::clock::Clock;
use crate::handler::{Disposition, JobContext, JobFailure, JobHandler};
use crate::retry::RetryPolicy;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Execution-owner name, e.g. `"desktop"`. Leases are taken as `"{node}:{instance}"`, and
    /// startup recovery only touches leases from earlier instances of the same node.
    pub node: String,
    pub lease_ttl: Duration,
    pub heartbeat_every: Duration,
    /// Longest sleep between queue checks when nothing wakes the worker.
    pub poll_interval: Duration,
    pub retry: RetryPolicy,
    pub overdue: OverduePolicy,
    /// Kinds this worker runs; empty means every registered kind.
    pub kinds: Vec<JobKind>,
    /// Jobs run concurrently by [`Worker::run`].
    pub concurrency: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            node: "desktop".to_owned(),
            lease_ttl: Duration::from_secs(60),
            heartbeat_every: Duration::from_secs(15),
            poll_interval: Duration::from_secs(5),
            retry: RetryPolicy::default(),
            overdue: OverduePolicy::default(),
            kinds: Vec::new(),
            concurrency: 2,
        }
    }
}

/// Claims jobs under a lease and runs them through registered [`JobHandler`]s.
pub struct Worker {
    store: Store,
    config: Mutex<WorkerConfig>,
    clock: Arc<dyn Clock>,
    handlers: HashMap<JobKind, Arc<dyn JobHandler>>,
    owner: String,
    wake: Arc<Notify>,
    rng: Mutex<StdRng>,
}

impl Worker {
    pub fn new(store: Store, config: WorkerConfig, clock: Arc<dyn Clock>) -> Self {
        let owner = format!("{}:{}", config.node, uuid::Uuid::new_v4());
        Self {
            store,
            config: Mutex::new(config),
            clock,
            handlers: HashMap::new(),
            owner,
            wake: Arc::new(Notify::new()),
            rng: Mutex::new(StdRng::from_os_rng()),
        }
    }

    pub fn register(&mut self, kind: JobKind, handler: Arc<dyn JobHandler>) {
        self.handlers.insert(kind, handler);
    }

    /// This process's lease owner string.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Notify after enqueuing a job to have the worker look immediately.
    pub fn waker(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// Changes the overdue policy for jobs claimed from now on.
    pub fn set_overdue_policy(&self, policy: OverduePolicy) {
        self.config.lock().expect("config lock").overdue = policy;
    }

    fn config(&self) -> WorkerConfig {
        self.config.lock().expect("config lock").clone()
    }

    fn kinds(&self) -> Vec<JobKind> {
        let configured = self.config().kinds;
        if configured.is_empty() {
            self.handlers.keys().copied().collect()
        } else {
            configured
        }
    }

    /// Startup recovery: requeue jobs left running by an earlier instance of this node.
    pub async fn recover(&self) -> StoreResult<Vec<Job>> {
        let node = self.config().node;
        let recovered = self
            .store
            .jobs()
            .recover_interrupted(&node, &self.owner, self.clock.now())
            .await?;
        for job in &recovered {
            info!(job = %job.id(), kind = %job.kind(), "recovered interrupted job");
        }
        Ok(recovered)
    }

    /// Claims and processes at most one job. Returns what happened to it, or `None` when
    /// nothing was due (or the lease was lost mid-run).
    pub async fn run_once(&self) -> StoreResult<Option<(Job, Disposition)>> {
        self.run_once_until(&CancellationToken::new()).await
    }

    async fn run_once_until(
        &self,
        shutdown: &CancellationToken,
    ) -> StoreResult<Option<(Job, Disposition)>> {
        let config = self.config();
        let kinds = self.kinds();
        if kinds.is_empty() {
            return Ok(None);
        }
        let ttl = chrono::Duration::from_std(config.lease_ttl).unwrap_or(chrono::Duration::MAX);
        let Some(job) = self
            .store
            .jobs()
            .claim_next(&self.owner, self.clock.now(), ttl, &kinds)
            .await?
        else {
            return Ok(None);
        };
        debug!(job = %job.id(), kind = %job.kind(), claims = job.claims(), "claimed job");
        Ok(self
            .process(job.clone(), &config, shutdown)
            .await?
            .map(|d| (job, d)))
    }

    async fn process(
        &self,
        job: Job,
        config: &WorkerConfig,
        shutdown: &CancellationToken,
    ) -> StoreResult<Option<Disposition>> {
        let Some(handler) = self.handlers.get(&job.kind()).cloned() else {
            let disposition = Disposition::Failed {
                message: format!("no handler registered for {}", job.kind()),
            };
            return self.settle(&job, None, disposition).await;
        };

        if job.is_overdue(&config.overdue, self.clock.now()) {
            let disposition = Disposition::Expired { due: job.run_at() };
            return self.settle(&job, Some(&handler), disposition).await;
        }

        let cancel = CancellationToken::new();
        let ctx = Arc::new(JobContext::new(
            self.store.clone(),
            &job,
            self.owner.clone(),
            self.clock.clone(),
            cancel.clone(),
        ));
        let heartbeat = self.spawn_heartbeat(&job, config, cancel.clone());

        let run = {
            let handler = handler.clone();
            let job = job.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move { handler.run(&job, &ctx).await })
        };
        let abort = run.abort_handle();
        let result = tokio::select! {
            joined = run => match joined {
                Ok(result) => result,
                Err(e) if e.is_panic() => Err(JobFailure::transient("job handler panicked")),
                Err(_) => Err(JobFailure::LeaseLost),
            },
            () = cancel.cancelled() => {
                abort.abort();
                Err(JobFailure::LeaseLost)
            }
            () = shutdown.cancelled() => {
                abort.abort();
                heartbeat.abort();
                self.release(&job).await;
                return Ok(None);
            }
        };
        heartbeat.abort();

        let disposition = match result {
            Ok(()) => Disposition::Succeeded,
            Err(JobFailure::LeaseLost) => {
                warn!(job = %job.id(), "lease lost; abandoning this run");
                return Ok(None);
            }
            Err(failure) => self.disposition_for(&job, failure, config),
        };
        self.settle(&job, Some(&handler), disposition).await
    }

    fn disposition_for(
        &self,
        job: &Job,
        failure: JobFailure,
        config: &WorkerConfig,
    ) -> Disposition {
        let now = self.clock.now();
        let mut rng = self.rng.lock().expect("rng lock");
        match failure {
            JobFailure::Transient {
                message,
                retry_after,
            } => {
                if job.retries_exhausted() {
                    Disposition::Failed {
                        message: format!(
                            "{message} (gave up after {} failed attempts)",
                            job.failures() + 1
                        ),
                    }
                } else {
                    let backoff = config.retry.backoff(job.failures() + 1, &mut *rng);
                    let delay = retry_after.map_or(backoff, |floor| backoff.max(floor));
                    Disposition::RetryAt {
                        at: now + to_chrono(delay),
                        error: message,
                        rate_limited: false,
                    }
                }
            }
            JobFailure::RateLimited {
                message,
                retry_after,
            } => Disposition::RetryAt {
                at: now + to_chrono(config.retry.rate_limit_delay(retry_after, &mut *rng)),
                error: message,
                rate_limited: true,
            },
            JobFailure::AuthRequired { message } => Disposition::Paused {
                reason: PauseReason::NeedsAuthentication,
                message,
            },
            JobFailure::Invalid { message } => Disposition::Paused {
                reason: PauseReason::NeedsUserAction,
                message,
            },
            JobFailure::OutcomeUnknown { message } => Disposition::Paused {
                reason: PauseReason::OutcomeUnknown,
                message,
            },
            JobFailure::Permanent { message } => Disposition::Failed { message },
            JobFailure::Abandon { message } => Disposition::Cancelled { message },
            JobFailure::LeaseLost => unreachable!("handled by the caller"),
        }
    }

    /// Lets the handler mirror the decision, then records it on the job.
    async fn settle(
        &self,
        job: &Job,
        handler: Option<&Arc<dyn JobHandler>>,
        disposition: Disposition,
    ) -> StoreResult<Option<Disposition>> {
        if let Some(handler) = handler {
            handler.on_settled(job, &disposition).await;
        }
        let owner = self.owner.as_str();
        let now = self.clock.now();
        let result = self
            .store
            .jobs()
            .modify(job.id(), |j| match &disposition {
                Disposition::Succeeded => j.succeed(owner, now),
                Disposition::RetryAt {
                    at,
                    error,
                    rate_limited,
                } => j.schedule_retry(owner, *at, error, !rate_limited, now),
                Disposition::Paused { reason, message } => j.pause(owner, *reason, message, now),
                Disposition::Failed { message } => j.fail(owner, message, now),
                Disposition::Expired { .. } => j.expire(owner, now),
                Disposition::Cancelled { .. } => j.cancel(now),
            })
            .await;
        match result {
            Ok(_) => {
                info!(job = %job.id(), kind = %job.kind(), ?disposition, "job settled");
                Ok(Some(disposition))
            }
            Err(e) if e.is_lease_lost() => {
                warn!(job = %job.id(), "lease lost before the result was recorded");
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn spawn_heartbeat(
        &self,
        job: &Job,
        config: &WorkerConfig,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let store = self.store.clone();
        let clock = self.clock.clone();
        let owner = self.owner.clone();
        let id = job.id();
        let every = config.heartbeat_every;
        let ttl = to_chrono(config.lease_ttl);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match store.jobs().renew(id, &owner, clock.now(), ttl).await {
                    Ok(_) => {}
                    Err(e) if e.is_lease_lost() => {
                        cancel.cancel();
                        return;
                    }
                    // Keep trying; the lease survives until its TTL runs out.
                    Err(e) => warn!(job = %id, error = %e, "heartbeat failed"),
                }
            }
        })
    }

    async fn release(&self, job: &Job) {
        let owner = self.owner.as_str();
        let now = self.clock.now();
        match self
            .store
            .jobs()
            .modify(job.id(), |j| j.release(owner, now))
            .await
        {
            Ok(_) => info!(job = %job.id(), "released job on shutdown"),
            Err(StoreError::Job(_)) => {}
            Err(e) => warn!(job = %job.id(), error = %e, "could not release job"),
        }
    }

    /// Runs until `shutdown` fires: recovers interrupted jobs, then processes due jobs with
    /// up to `concurrency` in flight, sleeping until the next due time, a wake-up or the poll
    /// interval. On shutdown, running jobs are released with their checkpoints.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        if let Err(e) = self.recover().await {
            warn!(error = %e, "startup recovery failed");
        }
        let slots = self.config().concurrency.max(1);
        let mut loops = Vec::with_capacity(slots);
        for _ in 0..slots {
            let worker = self.clone();
            let shutdown = shutdown.clone();
            loops.push(tokio::spawn(async move { worker.run_loop(shutdown).await }));
        }
        for l in loops {
            let _ = l.await;
        }
    }

    async fn run_loop(&self, shutdown: CancellationToken) {
        while !shutdown.is_cancelled() {
            match self.run_once_until(&shutdown).await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(e) => warn!(error = %e, "worker iteration failed"),
            }
            if shutdown.is_cancelled() {
                break;
            }
            let sleep = self.sleep_for().await;
            tokio::select! {
                () = tokio::time::sleep(sleep) => {}
                () = self.wake.notified() => {}
                () = shutdown.cancelled() => break,
            }
        }
    }

    async fn sleep_for(&self) -> Duration {
        let poll = self.config().poll_interval;
        let next = match self.store.jobs().next_due_at(&self.kinds()).await {
            Ok(next) => next,
            Err(_) => return poll,
        };
        next.map_or(poll, |at| until(at, self.clock.now()).min(poll))
    }
}

fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (at - now).to_std().unwrap_or(Duration::ZERO)
}

fn to_chrono(d: Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}
