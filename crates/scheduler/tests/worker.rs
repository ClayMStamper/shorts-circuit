//! Worker behaviour against a real store on a temp-file database.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use shorts_domain::{Job, JobKind, JobState, OverduePolicy, PauseReason, TimeZoneName};
use shorts_scheduler::{
    Clock, Disposition, JobContext, JobFailure, JobHandler, ManualClock, RetryPolicy, SystemClock,
    Worker, WorkerConfig, resolve_local,
};
use shorts_store::Store;
use tokio_util::sync::CancellationToken;

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn config() -> WorkerConfig {
    WorkerConfig {
        node: "desktop".into(),
        lease_ttl: Duration::from_secs(60),
        heartbeat_every: Duration::from_secs(20),
        poll_interval: Duration::from_millis(50),
        retry: RetryPolicy {
            base: Duration::from_secs(10),
            max: Duration::from_secs(600),
            max_failures: 3,
        },
        overdue: OverduePolicy::Expire { grace_secs: 900 },
        kinds: vec![],
        concurrency: 1,
    }
}

fn job_at(run_at: DateTime<Utc>, max_failures: u32) -> Job {
    Job::new(
        JobKind::PublishPublication,
        format!("subject-{}", uuid::Uuid::new_v4()),
        json!({}),
        run_at,
        true,
        max_failures,
        run_at,
    )
}

async fn open(dir: &Path) -> Store {
    Store::open(&dir.join("jobs.db")).await.unwrap()
}

/// Returns scripted results, one per run, and records what the worker decided.
#[derive(Default)]
struct Scripted {
    results: Mutex<VecDeque<Result<(), JobFailure>>>,
    runs: AtomicUsize,
    settled: Mutex<Vec<Disposition>>,
    panic_first: AtomicBool,
}

impl Scripted {
    fn new(results: Vec<Result<(), JobFailure>>) -> Arc<Self> {
        Arc::new(Self {
            results: Mutex::new(results.into()),
            ..Default::default()
        })
    }

    fn settled(&self) -> Vec<Disposition> {
        self.settled.lock().unwrap().clone()
    }
}

#[async_trait]
impl JobHandler for Scripted {
    async fn run(&self, _job: &Job, _ctx: &JobContext) -> Result<(), JobFailure> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if self.panic_first.swap(false, Ordering::SeqCst) {
            panic!("handler bug");
        }
        self.results.lock().unwrap().pop_front().unwrap_or(Ok(()))
    }

    async fn on_settled(&self, _job: &Job, disposition: &Disposition) {
        self.settled.lock().unwrap().push(disposition.clone());
    }
}

fn worker(store: &Store, clock: Arc<dyn Clock>, handler: Arc<dyn JobHandler>) -> Worker {
    worker_with(store, clock, handler, config())
}

fn worker_with(
    store: &Store,
    clock: Arc<dyn Clock>,
    handler: Arc<dyn JobHandler>,
    config: WorkerConfig,
) -> Worker {
    let mut w = Worker::new(store.clone(), config, clock);
    w.register(JobKind::PublishPublication, handler);
    w
}

#[tokio::test]
async fn successful_job_completes() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let clock = ManualClock::new(t0());
    let handler = Scripted::new(vec![Ok(())]);
    let w = worker(&store, Arc::new(clock), handler.clone());
    let job = job_at(t0(), 3);
    store.jobs().insert(&job).await.unwrap();

    let (ran, disposition) = w.run_once().await.unwrap().unwrap();
    assert_eq!(ran.id(), job.id());
    assert_eq!(disposition, Disposition::Succeeded);
    assert_eq!(handler.settled(), vec![Disposition::Succeeded]);
    assert_eq!(
        store.jobs().require(job.id()).await.unwrap().state(),
        JobState::Succeeded
    );
    assert!(w.run_once().await.unwrap().is_none());
}

#[tokio::test]
async fn jobs_are_not_run_before_they_are_due() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let clock = ManualClock::new(t0());
    let handler = Scripted::new(vec![]);
    let w = worker(&store, Arc::new(clock.clone()), handler.clone());
    store
        .jobs()
        .insert(&job_at(t0() + chrono::Duration::minutes(5), 3))
        .await
        .unwrap();
    assert!(w.run_once().await.unwrap().is_none());
    clock.advance(chrono::Duration::minutes(5));
    assert!(w.run_once().await.unwrap().is_some());
    assert_eq!(handler.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transient_failures_back_off_with_jitter_then_fail() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let clock = ManualClock::new(t0());
    let handler = Scripted::new(vec![
        Err(JobFailure::transient("connection reset")),
        Err(JobFailure::transient("503")),
        Err(JobFailure::transient("503 again")),
    ]);
    let w = worker(&store, Arc::new(clock.clone()), handler.clone());
    let job = job_at(t0(), 3);
    store.jobs().insert(&job).await.unwrap();

    // Attempt 1 fails: retry 5-10 s later (base 10 s, equal jitter).
    let (_, d) = w.run_once().await.unwrap().unwrap();
    let Disposition::RetryAt { at, .. } = d else {
        panic!("expected retry, got {d:?}")
    };
    let delay = at - clock.now();
    assert!(delay >= chrono::Duration::seconds(5) && delay <= chrono::Duration::seconds(10));
    assert!(w.run_once().await.unwrap().is_none(), "not due yet");

    // Attempt 2 fails: 10-20 s.
    clock.set(at);
    let (_, d) = w.run_once().await.unwrap().unwrap();
    let Disposition::RetryAt { at, .. } = d else {
        panic!("expected retry, got {d:?}")
    };
    let delay = at - clock.now();
    assert!(delay >= chrono::Duration::seconds(10) && delay <= chrono::Duration::seconds(20));
    assert_eq!(store.jobs().require(job.id()).await.unwrap().failures(), 2);

    // Attempt 3 exhausts the budget.
    clock.set(at);
    let (_, d) = w.run_once().await.unwrap().unwrap();
    assert!(matches!(d, Disposition::Failed { ref message } if message.contains("gave up")));
    let stored = store.jobs().require(job.id()).await.unwrap();
    assert_eq!(stored.state(), JobState::Failed);
    assert_eq!(stored.claims(), 3);
    assert_eq!(handler.settled().len(), 3);
}

#[tokio::test]
async fn retry_after_is_a_floor_for_transient_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let clock = ManualClock::new(t0());
    let handler = Scripted::new(vec![Err(JobFailure::Transient {
        message: "busy".into(),
        retry_after: Some(Duration::from_secs(300)),
    })]);
    let w = worker(&store, Arc::new(clock.clone()), handler);
    store.jobs().insert(&job_at(t0(), 3)).await.unwrap();
    let (_, d) = w.run_once().await.unwrap().unwrap();
    let Disposition::RetryAt { at, .. } = d else {
        panic!()
    };
    assert!(at - t0() >= chrono::Duration::seconds(300));
}

#[tokio::test]
async fn rate_limits_are_honoured_without_spending_retries() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let clock = ManualClock::new(t0());
    let handler = Scripted::new(vec![
        Err(JobFailure::RateLimited {
            message: "429".into(),
            retry_after: Duration::from_secs(120),
        }),
        Err(JobFailure::RateLimited {
            message: "429".into(),
            retry_after: Duration::from_secs(120),
        }),
        Ok(()),
    ]);
    let w = worker(&store, Arc::new(clock.clone()), handler);
    let job = job_at(t0(), 1);
    store.jobs().insert(&job).await.unwrap();

    for _ in 0..2 {
        let (_, d) = w.run_once().await.unwrap().unwrap();
        let Disposition::RetryAt {
            at, rate_limited, ..
        } = d
        else {
            panic!("expected retry, got {d:?}")
        };
        assert!(rate_limited);
        let delay = at - clock.now();
        assert!(delay >= chrono::Duration::seconds(120));
        assert!(delay <= chrono::Duration::seconds(132));
        clock.advance(chrono::Duration::seconds(60));
        assert!(
            w.run_once().await.unwrap().is_none(),
            "must wait out the limit"
        );
        clock.set(at);
    }
    // max_failures = 1, yet two rate limits didn't exhaust it.
    let (_, d) = w.run_once().await.unwrap().unwrap();
    assert_eq!(d, Disposition::Succeeded);
    assert_eq!(store.jobs().require(job.id()).await.unwrap().failures(), 0);
}

#[tokio::test]
async fn auth_validation_and_unknown_outcomes_pause_for_the_user() {
    let cases = [
        (
            JobFailure::AuthRequired {
                message: "token revoked".into(),
            },
            PauseReason::NeedsAuthentication,
        ),
        (
            JobFailure::Invalid {
                message: "video too long".into(),
            },
            PauseReason::NeedsUserAction,
        ),
        (
            JobFailure::OutcomeUnknown {
                message: "publish response lost".into(),
            },
            PauseReason::OutcomeUnknown,
        ),
    ];
    for (failure, reason) in cases {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        let clock = ManualClock::new(t0());
        let handler = Scripted::new(vec![Err(failure)]);
        let w = worker(&store, Arc::new(clock.clone()), handler.clone());
        let job = job_at(t0(), 3);
        store.jobs().insert(&job).await.unwrap();

        let (_, d) = w.run_once().await.unwrap().unwrap();
        assert!(matches!(d, Disposition::Paused { reason: r, .. } if r == reason));
        let stored = store.jobs().require(job.id()).await.unwrap();
        assert_eq!(stored.state(), JobState::Paused);
        assert_eq!(stored.pause_reason(), Some(reason));

        // Never picked up again on its own.
        clock.advance(chrono::Duration::days(365));
        assert!(w.run_once().await.unwrap().is_none());
        assert_eq!(handler.runs.load(Ordering::SeqCst), 1);

        // Once the user resumes it, it runs again.
        store
            .jobs()
            .modify(job.id(), |j| j.resume(clock.now(), clock.now()))
            .await
            .unwrap();
        assert_eq!(
            w.run_once().await.unwrap().unwrap().1,
            Disposition::Succeeded
        );
    }
}

#[tokio::test]
async fn abandoned_jobs_are_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let handler = Scripted::new(vec![Err(JobFailure::Abandon {
        message: "publication was cancelled".into(),
    })]);
    let w = worker(&store, Arc::new(ManualClock::new(t0())), handler);
    let job = job_at(t0(), 3);
    store.jobs().insert(&job).await.unwrap();
    w.run_once().await.unwrap().unwrap();
    assert_eq!(
        store.jobs().require(job.id()).await.unwrap().state(),
        JobState::Cancelled
    );
}

#[tokio::test]
async fn handler_panics_are_retried() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let handler = Scripted::new(vec![]);
    handler.panic_first.store(true, Ordering::SeqCst);
    let w = worker(&store, Arc::new(ManualClock::new(t0())), handler);
    store.jobs().insert(&job_at(t0(), 3)).await.unwrap();
    let (_, d) = w.run_once().await.unwrap().unwrap();
    assert!(matches!(d, Disposition::RetryAt { ref error, .. } if error.contains("panicked")));
}

#[tokio::test]
async fn overdue_jobs_expire_under_the_expiry_policy() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    // The app was closed: it is now 2 hours past the scheduled time.
    let clock = ManualClock::new(t0() + chrono::Duration::hours(2));
    let handler = Scripted::new(vec![]);
    let w = worker(&store, Arc::new(clock), handler.clone());
    let job = job_at(t0(), 3);
    store.jobs().insert(&job).await.unwrap();

    let (_, d) = w.run_once().await.unwrap().unwrap();
    assert_eq!(d, Disposition::Expired { due: t0() });
    assert_eq!(
        handler.runs.load(Ordering::SeqCst),
        0,
        "expired work must not run"
    );
    assert_eq!(handler.settled(), vec![Disposition::Expired { due: t0() }]);
    assert_eq!(
        store.jobs().require(job.id()).await.unwrap().state(),
        JobState::Expired
    );
}

#[tokio::test]
async fn overdue_jobs_within_grace_or_under_catch_up_still_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let clock = ManualClock::new(t0() + chrono::Duration::minutes(10));
    let handler = Scripted::new(vec![]);
    let w = worker(&store, Arc::new(clock.clone()), handler.clone());
    store.jobs().insert(&job_at(t0(), 3)).await.unwrap();
    assert_eq!(
        w.run_once().await.unwrap().unwrap().1,
        Disposition::Succeeded
    );

    w.set_overdue_policy(OverduePolicy::CatchUp);
    store.jobs().insert(&job_at(t0(), 3)).await.unwrap();
    clock.advance(chrono::Duration::days(3));
    assert_eq!(
        w.run_once().await.unwrap().unwrap().1,
        Disposition::Succeeded
    );
    assert_eq!(handler.runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn untimed_jobs_never_expire() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let clock = ManualClock::new(t0() + chrono::Duration::days(2));
    let handler = Scripted::new(vec![]);
    let mut w = Worker::new(store.clone(), config(), Arc::new(clock));
    w.register(JobKind::RenderVariant, handler);
    let job = Job::new(
        JobKind::RenderVariant,
        "v1",
        json!({}),
        t0(),
        false,
        3,
        t0(),
    );
    store.jobs().insert(&job).await.unwrap();
    assert_eq!(
        w.run_once().await.unwrap().unwrap().1,
        Disposition::Succeeded
    );
}

#[tokio::test]
async fn scheduled_local_time_runs_at_the_right_utc_instant_across_dst() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let tz = TimeZoneName::parse("America/New_York").unwrap();
    // 09:00 on the day US clocks fall back is 14:00Z (EST), not 13:00Z (EDT).
    let local = NaiveDateTime::parse_from_str("2026-11-01 09:00", "%Y-%m-%d %H:%M").unwrap();
    let due = resolve_local(tz, local);
    assert_eq!(due.to_rfc3339(), "2026-11-01T14:00:00+00:00");

    let clock = ManualClock::new(due - chrono::Duration::minutes(30));
    let handler = Scripted::new(vec![]);
    let w = worker(&store, Arc::new(clock.clone()), handler);
    store.jobs().insert(&job_at(due, 3)).await.unwrap();
    assert!(
        w.run_once().await.unwrap().is_none(),
        "13:30Z is 08:30 local"
    );
    clock.set(due);
    assert!(w.run_once().await.unwrap().is_some());
}

// --- checkpoints, crashes and leases -----------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct StepCheckpoint {
    done: u32,
}

/// Five steps with a checkpoint after each. Optionally hangs (as if the process froze or was
/// killed) after `hang_after` steps on its first run.
struct Steps {
    executed: Mutex<Vec<u32>>,
    saved: AtomicUsize,
    hang_after: Mutex<Option<u32>>,
}

impl Steps {
    fn new(hang_after: Option<u32>) -> Arc<Self> {
        Arc::new(Self {
            executed: Mutex::new(Vec::new()),
            saved: AtomicUsize::new(0),
            hang_after: Mutex::new(hang_after),
        })
    }

    fn executed(&self) -> Vec<u32> {
        self.executed.lock().unwrap().clone()
    }
}

#[async_trait]
impl JobHandler for Steps {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> Result<(), JobFailure> {
        let start = ctx.checkpoint::<StepCheckpoint>().unwrap_or_default().done;
        for step in start + 1..=5 {
            self.executed.lock().unwrap().push(step);
            ctx.save_checkpoint(&StepCheckpoint { done: step }).await?;
            self.saved.store(step as usize, Ordering::SeqCst);
            // Hang at most once, like a single crash.
            let hang = {
                let mut hang_after = self.hang_after.lock().unwrap();
                *hang_after == Some(step) && hang_after.take().is_some()
            };
            if hang {
                std::future::pending::<()>().await;
            }
        }
        Ok(())
    }
}

/// Runs the job on a separate runtime until three steps are checkpointed, then kills that
/// runtime without any cleanup, like a crashed process.
fn crash_midway(db_dir: PathBuf, clock: Arc<dyn Clock>, handler: Arc<Steps>, node: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let node = node.to_owned();
    let h = handler.clone();
    rt.spawn(async move {
        let store = open(&db_dir).await;
        let w = worker_with(&store, clock, h, WorkerConfig { node, ..config() });
        let _ = w.run_once().await;
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while handler.saved.load(Ordering::SeqCst) < 3 {
        assert!(std::time::Instant::now() < deadline, "worker never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    rt.shutdown_background();
}

#[test]
fn restart_recovers_interrupted_job_from_its_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(t0()));
    let job = job_at(t0(), 3);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        open(dir.path()).await.jobs().insert(&job).await.unwrap();
    });

    let handler = Steps::new(Some(3));
    crash_midway(
        dir.path().to_owned(),
        clock.clone(),
        handler.clone(),
        "desktop",
    );
    assert_eq!(handler.executed(), vec![1, 2, 3]);

    rt.block_on(async {
        // A fresh process: new store connection, new worker instance, same node.
        let store = open(dir.path()).await;
        let stored = store.jobs().require(job.id()).await.unwrap();
        assert_eq!(
            stored.state(),
            JobState::Running,
            "left running by the crash"
        );

        let w = worker(&store, clock.clone(), handler.clone());
        let recovered = w.recover().await.unwrap();
        assert_eq!(recovered.len(), 1);
        let (_, d) = w.run_once().await.unwrap().unwrap();
        assert_eq!(d, Disposition::Succeeded);

        let stored = store.jobs().require(job.id()).await.unwrap();
        assert_eq!(stored.state(), JobState::Succeeded);
        assert_eq!(stored.claims(), 2);
        assert_eq!(stored.failures(), 0, "an interruption is not a failure");
    });
    // Steps 1-3 were not repeated.
    assert_eq!(handler.executed(), vec![1, 2, 3, 4, 5]);
}

#[test]
fn another_owner_takes_over_after_the_lease_expires() {
    let dir = tempfile::tempdir().unwrap();
    let manual = ManualClock::new(t0());
    let clock: Arc<dyn Clock> = Arc::new(manual.clone());
    let job = job_at(t0(), 3);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        open(dir.path()).await.jobs().insert(&job).await.unwrap();
    });

    let handler = Steps::new(Some(3));
    crash_midway(
        dir.path().to_owned(),
        clock.clone(),
        handler.clone(),
        "desktop",
    );

    rt.block_on(async {
        let store = open(dir.path()).await;
        // A different node: recovery doesn't apply, only lease expiry does.
        let w = worker_with(
            &store,
            clock.clone(),
            handler.clone(),
            WorkerConfig {
                node: "other".into(),
                ..config()
            },
        );
        assert!(w.recover().await.unwrap().is_empty());
        assert!(w.run_once().await.unwrap().is_none(), "lease still live");
        manual.advance(chrono::Duration::seconds(61));
        let (_, d) = w.run_once().await.unwrap().unwrap();
        assert_eq!(d, Disposition::Succeeded);
    });
    assert_eq!(handler.executed(), vec![1, 2, 3, 4, 5]);
}

struct Sleeper {
    duration: Duration,
    cancelled_seen: AtomicBool,
    settled: AtomicUsize,
}

#[async_trait]
impl JobHandler for Sleeper {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> Result<(), JobFailure> {
        tokio::select! {
            () = tokio::time::sleep(self.duration) => Ok(()),
            () = ctx.cancelled() => {
                self.cancelled_seen.store(true, Ordering::SeqCst);
                Err(JobFailure::LeaseLost)
            }
        }
    }

    async fn on_settled(&self, _job: &Job, _d: &Disposition) {
        self.settled.fetch_add(1, Ordering::SeqCst);
    }
}

fn fast_lease() -> WorkerConfig {
    WorkerConfig {
        lease_ttl: Duration::from_millis(600),
        heartbeat_every: Duration::from_millis(100),
        ..config()
    }
}

#[tokio::test]
async fn heartbeats_keep_long_jobs_leased() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let handler = Arc::new(Sleeper {
        duration: Duration::from_millis(2000),
        cancelled_seen: AtomicBool::new(false),
        settled: AtomicUsize::new(0),
    });
    let w = Arc::new(worker_with(
        &store,
        Arc::new(SystemClock),
        handler,
        fast_lease(),
    ));
    let job = job_at(Utc::now(), 3);
    store.jobs().insert(&job).await.unwrap();

    let running = tokio::spawn({
        let w = w.clone();
        async move { w.run_once().await.unwrap() }
    });
    wait_for_state(&store, &job, JobState::Running).await;
    // Meanwhile a rival keeps trying to claim it; the lease (600 ms) is only alive because of
    // heartbeats.
    let rival = Worker::new(store.clone(), fast_lease(), Arc::new(SystemClock));
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1600);
    while tokio::time::Instant::now() < deadline {
        let stolen = store
            .jobs()
            .claim_next(
                rival.owner(),
                Utc::now(),
                chrono::Duration::milliseconds(600),
                &[],
            )
            .await
            .unwrap();
        assert!(stolen.is_none(), "lease expired despite heartbeats");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let (_, d) = running.await.unwrap().unwrap();
    assert_eq!(d, Disposition::Succeeded);
}

#[tokio::test]
async fn losing_the_lease_stops_the_handler_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let handler = Arc::new(Sleeper {
        duration: Duration::from_secs(30),
        cancelled_seen: AtomicBool::new(false),
        settled: AtomicUsize::new(0),
    });
    let w = Arc::new(worker_with(
        &store,
        Arc::new(SystemClock),
        handler.clone(),
        fast_lease(),
    ));
    let job = job_at(Utc::now(), 3);
    store.jobs().insert(&job).await.unwrap();
    let running = tokio::spawn({
        let w = w.clone();
        async move { w.run_once().await.unwrap() }
    });
    wait_for_state(&store, &job, JobState::Running).await;
    // The user cancels it from the UI.
    store
        .jobs()
        .modify(job.id(), |j| j.cancel(Utc::now()))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("handler should stop promptly")
        .unwrap();
    assert!(outcome.is_none());
    assert_eq!(handler.settled.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.jobs().require(job.id()).await.unwrap().state(),
        JobState::Cancelled
    );
}

async fn wait_for_state(store: &Store, job: &Job, state: JobState) -> Job {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let current = store.jobs().require(job.id()).await.unwrap();
        if current.state() == state {
            return current;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job stuck in {}",
            current.state()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn run_loop_processes_work_and_shuts_down_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let handler = Scripted::new(vec![]);
    let w = Arc::new(worker_with(
        &store,
        Arc::new(SystemClock),
        handler.clone(),
        WorkerConfig {
            poll_interval: Duration::from_secs(30),
            concurrency: 2,
            ..config()
        },
    ));
    let waker = w.waker();
    let shutdown = CancellationToken::new();
    let looping = tokio::spawn(w.clone().run(shutdown.clone()));

    let job = job_at(Utc::now(), 3);
    store.jobs().insert(&job).await.unwrap();
    waker.notify_one();
    wait_for_state(&store, &job, JobState::Succeeded).await;

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), looping)
        .await
        .expect("loop should stop")
        .unwrap();
}

#[tokio::test]
async fn shutdown_releases_running_jobs_with_their_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path()).await;
    let handler = Steps::new(Some(2));
    let w = Arc::new(worker_with(
        &store,
        Arc::new(SystemClock),
        handler.clone(),
        config(),
    ));
    let shutdown = CancellationToken::new();
    let looping = tokio::spawn(w.clone().run(shutdown.clone()));
    let job = job_at(Utc::now(), 3);
    store.jobs().insert(&job).await.unwrap();
    w.waker().notify_one();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while handler.saved.load(Ordering::SeqCst) < 2 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    shutdown.cancel();
    looping.await.unwrap();

    let stored = store.jobs().require(job.id()).await.unwrap();
    assert_eq!(stored.state(), JobState::Queued);
    assert_eq!(stored.checkpoint(), Some(&json!({"done": 2})));
    assert_eq!(stored.failures(), 0);
}
