//! Ties the runner together: the PostgreSQL store, uploaded media, adapters with their token
//! sources, the durable worker (the same scheduler and publish pipeline as the desktop), the
//! sync API and housekeeping.

use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use shorts_adapters::google::{GoogleEndpoints, OAuthClient};
use shorts_adapters::instagram::{InstagramAdapter, InstagramConfig};
use shorts_adapters::linkedin::{LinkedInAdapter, LinkedInConfig};
use shorts_adapters::snapchat::{SnapAdapter, SnapConfig, SnapEndpoints, SnapOAuth};
use shorts_adapters::youtube::{YouTubeAdapter, YouTubeConfig};
use shorts_adapters::{AdapterRegistry, ErasedAdapter};
use shorts_domain::{JobKind, OverduePolicy};
use shorts_publish::PublishHandler;
use shorts_scheduler::{Clock, Worker, WorkerConfig};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::db::PgStore;
use crate::host::{PgQuota, RunnerHost};
use crate::media::MediaDir;
use crate::tokens::RunnerTokens;

/// How long an unreferenced upload is kept, so a hand-over whose video just arrived isn't
/// swept before its publication follows.
const UPLOAD_RETENTION: chrono::Duration = chrono::Duration::hours(6);
const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(10 * 60);
const INSTAGRAM_RENEW_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Where the platform APIs are. Tests point them at local fakes.
#[derive(Debug, Clone)]
pub struct RunnerSetup {
    pub google: GoogleEndpoints,
    pub youtube: YouTubeConfig,
    pub instagram: InstagramConfig,
    pub linkedin: LinkedInConfig,
    pub snap_oauth: SnapEndpoints,
    pub snap: SnapConfig,
    /// Per-request timeout for provider calls.
    pub request_timeout: Duration,
    /// What to do with publications found overdue (the runner was down at the time).
    pub overdue: OverduePolicy,
}

impl Default for RunnerSetup {
    fn default() -> Self {
        Self {
            google: GoogleEndpoints::default(),
            youtube: YouTubeConfig::default(),
            instagram: InstagramConfig::default(),
            linkedin: LinkedInConfig::default(),
            snap_oauth: SnapEndpoints::default(),
            snap: SnapConfig::default(),
            request_timeout: Duration::from_secs(300),
            overdue: OverduePolicy::default(),
        }
    }
}

impl RunnerSetup {
    /// The defaults, with the same overrides the desktop reads (`SHORTS_INSTAGRAM_API_VERSION`,
    /// `SHORTS_LINKEDIN_API_VERSION`, `SHORTS_YOUTUBE_UPLOADS_PER_DAY`,
    /// `SHORTS_YOUTUBE_UNITS_PER_DAY`) plus `SHORTS_RUNNER_OVERDUE`: `catch-up`, or the grace
    /// period in minutes (default 15) after which an overdue publication waits for the user.
    pub fn from_env() -> Self {
        let mut setup = Self::default();
        let var = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let num = |key: &str| {
            var(key)
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|n| *n > 0)
        };
        if let Some(v) = var("SHORTS_INSTAGRAM_API_VERSION").filter(|v| v.starts_with('v')) {
            setup.instagram.api_version = v;
        }
        if let Some(v) = var("SHORTS_LINKEDIN_API_VERSION").filter(|v| v.len() == 6) {
            setup.linkedin.api_version = v;
        }
        if let Some(n) = num("SHORTS_YOUTUBE_UPLOADS_PER_DAY") {
            setup.youtube.limits.uploads_per_day = n;
        }
        if let Some(n) = num("SHORTS_YOUTUBE_UNITS_PER_DAY") {
            setup.youtube.limits.units_per_day = n;
        }
        match var("SHORTS_RUNNER_OVERDUE").as_deref() {
            Some("catch-up" | "catch_up") => setup.overdue = OverduePolicy::CatchUp,
            Some(minutes) => {
                if let Ok(m) = minutes.parse::<u64>() {
                    setup.overdue = OverduePolicy::Expire { grace_secs: m * 60 };
                }
            }
            None => {}
        }
        setup
    }
}

pub struct Runner {
    pub(crate) store: PgStore,
    pub(crate) media: MediaDir,
    pub(crate) tokens: Arc<RunnerTokens>,
    adapters: Arc<RwLock<AdapterRegistry>>,
    pub(crate) clock: Arc<dyn Clock>,
    overdue: OverduePolicy,
    wake: OnceLock<Arc<Notify>>,
}

impl Runner {
    /// Builds the runner with an adapter for every platform.
    pub fn new(
        store: PgStore,
        media: MediaDir,
        setup: RunnerSetup,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(setup.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        let tokens = RunnerTokens::new(
            store.clone(),
            clock.clone(),
            OAuthClient::new(http.clone(), setup.google.clone()),
            SnapOAuth::new(http.clone(), setup.snap_oauth.clone()),
        );
        let now = {
            let clock = clock.clone();
            Arc::new(move || clock.now())
        };
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(YouTubeAdapter::new(
            http.clone(),
            setup.youtube.clone(),
            tokens.clone(),
            Arc::new(PgQuota {
                store: store.clone(),
                clock: clock.clone(),
            }),
            now.clone(),
        )));
        let instagram = Arc::new(InstagramAdapter::new(
            http.clone(),
            setup.instagram.clone(),
            tokens.clone(),
            now.clone(),
        ));
        tokens.set_instagram(instagram.clone());
        registry.register(instagram);
        registry.register(Arc::new(LinkedInAdapter::new(
            http.clone(),
            setup.linkedin.clone(),
            tokens.clone(),
            now.clone(),
        )));
        registry.register(Arc::new(SnapAdapter::new(
            http,
            setup.snap.clone(),
            tokens.clone(),
            now,
        )));
        Arc::new(Self {
            store,
            media,
            tokens,
            adapters: Arc::new(RwLock::new(registry)),
            clock,
            overdue: setup.overdue,
            wake: OnceLock::new(),
        })
    }

    pub fn store(&self) -> &PgStore {
        &self.store
    }

    pub fn media(&self) -> &MediaDir {
        &self.media
    }

    pub fn tokens(&self) -> &Arc<RunnerTokens> {
        &self.tokens
    }

    /// Replaces the adapter for its platform (tests use a fake).
    pub fn register_adapter(&self, adapter: Arc<dyn ErasedAdapter>) {
        self.adapters
            .write()
            .expect("adapters lock")
            .register(adapter);
    }

    /// The durable worker, running the shared publish pipeline as the cloud executor. Lease
    /// owners are `cloud:<instance>`.
    pub fn worker(&self, mut config: WorkerConfig) -> Arc<Worker> {
        config.node = "cloud".to_owned();
        config.kinds = vec![JobKind::PublishPublication];
        config.overdue = self.overdue;
        let mut worker = Worker::new(self.store.clone(), config, self.clock.clone());
        worker.register(
            JobKind::PublishPublication,
            Arc::new(PublishHandler::new(Arc::new(RunnerHost {
                store: self.store.clone(),
                media: self.media.clone(),
                adapters: self.adapters.clone(),
                clock: self.clock.clone(),
            }))),
        );
        let _ = self.wake.set(worker.waker());
        Arc::new(worker)
    }

    /// Has the worker look at the queue now (after a hand-over or reconnect).
    pub(crate) fn poke(&self) {
        if let Some(wake) = self.wake.get() {
            wake.notify_one();
        }
    }

    /// Deletes media no live publication needs any more.
    pub async fn sweep_media(&self) {
        let before = self.clock.now() - UPLOAD_RETENTION;
        let expired = match self.store.expired_uploads(before).await {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "could not list expired uploads");
                return;
            }
        };
        for (installation, sha) in expired {
            if let Err(e) = self.media.delete(installation, &sha).await {
                warn!(error = %e, "could not delete an expired upload");
                continue;
            }
            if let Err(e) = self.store.delete_upload(installation, &sha).await {
                warn!(error = %e, "could not forget an expired upload");
            } else {
                info!(%installation, "deleted media no publication needs");
            }
        }
    }

    async fn housekeeping(self: Arc<Self>, shutdown: CancellationToken) {
        let mut since_renewal = INSTAGRAM_RENEW_EVERY;
        loop {
            self.sweep_media().await;
            if since_renewal >= INSTAGRAM_RENEW_EVERY {
                since_renewal = Duration::ZERO;
                match self.store.instagram_accounts_in_use().await {
                    Ok(accounts) => self.tokens.renew_instagram(&accounts).await,
                    Err(e) => warn!(error = %e, "could not list Instagram accounts"),
                }
            }
            tokio::select! {
                () = tokio::time::sleep(HOUSEKEEPING_EVERY) => since_renewal += HOUSEKEEPING_EVERY,
                () = shutdown.cancelled() => return,
            }
        }
    }

    /// Serves the sync API on `listener` and runs the worker and housekeeping until
    /// `shutdown`; running jobs are released with their checkpoints.
    pub async fn serve(
        self: Arc<Self>,
        listener: tokio::net::TcpListener,
        worker: WorkerConfig,
        shutdown: CancellationToken,
    ) -> std::io::Result<()> {
        let worker = self.worker(worker);
        let worker_task = tokio::spawn(worker.run(shutdown.clone()));
        let housekeeping = tokio::spawn(self.clone().housekeeping(shutdown.clone()));
        let app = crate::api::router(self.clone());
        let stop = shutdown.clone();
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
        shutdown.cancel();
        let _ = worker_task.await;
        let _ = housekeeping.await;
        served
    }
}
