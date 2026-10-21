//! Optional TikTok connector: detects new posts so their originals can be matched when they
//! land in the watch folder (and workflows use the post's caption). It is off unless the owner
//! turns it on, and unavailable (with the next step) until a TikTok developer app is set up.
//!
//! Two ways to sign in:
//! - **Direct** (preferred): the owner pastes their TikTok app's client key and secret (kept in
//!   the OS credential store); the browser sign-in returns to a loopback address and the app
//!   exchanges and refreshes tokens with TikTok itself (`shorts_adapters::tiktok_oauth`).
//! - **Broker**: when `SHORTS_BROKER_URL` points at the OAuth broker and no app is pasted.
//!
//! Only metadata is read (`user.info.basic`, `video.list`). No video file is ever downloaded
//! from TikTok; the only bytes fetched are cover images.

use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use shorts_adapters::broker::{BrokerClient, Claim, TokenSet, base64url, challenge_for};
use shorts_adapters::google::Loopback;
use shorts_adapters::tiktok::{
    DISPLAY_API_BASE, DisplayApi, IntakeError, MAX_PAGE_SIZE, fetch_cover,
};
use shorts_adapters::tiktok_oauth::{ChallengeEncoding, TikTokApp, TikTokEndpoints, TikTokOAuth};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::app::App;
use crate::credentials::CredentialStore;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;

const ENABLED_KEY: &str = "tiktok_enabled";
const SOURCE_KEY: &str = "tiktok_source";
const CREDENTIAL_KEY: &str = "tiktok";
/// The owner's TikTok developer app (client key and secret).
const APP_KEY: &str = "tiktok_app";
/// The fixed loopback redirect registered in the TikTok app: `http://127.0.0.1:47616/callback/`.
pub const DEFAULT_LOOPBACK_PORT: u16 = 47616;
pub const LOOPBACK_PATH: &str = "/callback/";
/// How far back the first check looks.
const FIRST_LOOKBACK_DAYS: i64 = 7;
const MAX_PAGES_PER_CHECK: usize = 5;
const COVER_MAX_BYTES: usize = 5 * 1024 * 1024;
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(600);
/// Refresh the access token when it has less than this left.
const REFRESH_MARGIN_SECS: i64 = 300;

pub const REGISTER_NEXT_STEP: &str = "Register a TikTok developer app with Login Kit (desktop) and \
    the user.info.basic and video.list scopes, add the redirect URI \
    http://127.0.0.1:47616/callback/, then paste its client key and secret here (see \
    docs/human-scope.md).";

/// Where the connector's services are. Absent means the feature is unavailable.
#[derive(Debug, Clone)]
pub struct TikTokSetup {
    /// The OAuth broker, when one is used instead of the owner's own app.
    pub broker_url: Option<Url>,
    pub api_base: Url,
    /// TikTok's sign-in endpoints for the direct flow.
    pub oauth: TikTokEndpoints,
    /// Loopback port for the direct sign-in (`0` picks a free one, for tests).
    pub loopback_port: u16,
    pub challenge: ChallengeEncoding,
    /// Time between automatic checks while the app runs.
    pub poll_every: StdDuration,
}

impl TikTokSetup {
    /// The direct flow against TikTok, plus the broker when `SHORTS_BROKER_URL` is set.
    /// `SHORTS_TIKTOK_API_BASE` overrides the API host.
    pub fn from_env() -> Self {
        let broker_url = std::env::var("SHORTS_BROKER_URL")
            .ok()
            .and_then(|b| Url::parse(b.trim()).ok());
        let api_base = std::env::var("SHORTS_TIKTOK_API_BASE")
            .ok()
            .and_then(|u| Url::parse(u.trim()).ok())
            .unwrap_or_else(|| Url::parse(DISPLAY_API_BASE).expect("valid default"));
        Self {
            broker_url,
            api_base,
            oauth: TikTokEndpoints::default(),
            loopback_port: DEFAULT_LOOPBACK_PORT,
            challenge: ChallengeEncoding::from_env(),
            poll_every: StdDuration::from_secs(15 * 60),
        }
    }
}

/// How the TikTok app is set up, for the Connections page. Never includes the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TikTokAppInfo {
    /// The owner's client key and secret are saved.
    pub app_configured: bool,
    pub client_key: Option<String>,
    /// An OAuth broker is configured (used when no app is pasted).
    pub broker_configured: bool,
    /// The redirect URI to register in the TikTok app.
    pub redirect_uri: String,
}

/// The connected TikTok account, kept in settings. Tokens are in the credential store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Source {
    open_id: String,
    display_name: String,
    connected_at: DateTime<Utc>,
    /// Posts older than this are ignored.
    since: DateTime<Utc>,
    last_checked_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    needs_reconnect: bool,
    /// Signed in through the broker (connections made before the direct flow existed were).
    #[serde(default = "via_broker_default")]
    via_broker: bool,
}

fn via_broker_default() -> bool {
    true
}

/// Whether new-post detection is on and signed in, read from settings (so the core can tell
/// without the connector).
pub(crate) async fn detection_active(app: &App) -> AppResult<bool> {
    let settings = app.store.settings();
    let enabled = settings.get::<bool>(ENABLED_KEY).await?.unwrap_or(false);
    let source: Option<Source> = settings.get(SOURCE_KEY).await?;
    Ok(enabled && source.is_some_and(|s| !s.needs_reconnect))
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredTokens {
    access_token: String,
    access_expires_at: DateTime<Utc>,
    refresh_token: String,
}

impl StoredTokens {
    fn from_set(t: &TokenSet, now: DateTime<Utc>) -> Self {
        Self {
            access_token: t.access_token.clone(),
            access_expires_at: now + Duration::seconds(t.expires_in.max(0)),
            refresh_token: t.refresh_token.clone(),
        }
    }
}

/// What the Settings screen shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TikTokStatus {
    /// The feature can't be used yet; `next_step` says what would fix that.
    Unavailable {
        reason: String,
        next_step: String,
    },
    /// Available but turned off (the default).
    Off,
    NotConnected,
    /// Waiting for the browser sign-in to finish.
    Connecting,
    Connected {
        display_name: String,
        connected_at: DateTime<Utc>,
        last_checked_at: Option<DateTime<Utc>>,
        last_error: Option<String>,
        /// Posts still waiting for their original file.
        pending_posts: usize,
    },
    /// TikTok refused the stored tokens; connect again.
    Reconnect {
        display_name: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConnectProgress {
    Pending,
    Connected { status: TikTokStatus },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    pub new_posts: usize,
    pub suggestions: usize,
}

struct PendingConnect {
    state: String,
    verifier: String,
    started: Instant,
    /// Direct sign-ins finish in the background; the result lands here.
    direct: Option<Arc<Mutex<Option<ConnectProgress>>>>,
    /// Stops a direct sign-in's loopback listener (freeing its port) when this sign-in is
    /// replaced, finished or abandoned.
    cancel: CancellationToken,
}

impl Drop for PendingConnect {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Clients {
    broker: Option<BrokerClient>,
    oauth: TikTokOAuth,
    api: DisplayApi,
    loopback_port: u16,
    poll_every: StdDuration,
}

/// The TikTok new-post detector.
pub struct TikTokIntake {
    app: Arc<App>,
    clients: Option<Clients>,
    http: reqwest::Client,
    credentials: Arc<dyn CredentialStore>,
    pending: Mutex<Option<PendingConnect>>,
    check_lock: tokio::sync::Mutex<()>,
    wake: Notify,
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64url(&bytes)
}

fn intake_error(e: IntakeError) -> AppError {
    AppError::Intake(e.to_string())
}

impl TikTokIntake {
    pub fn new(
        app: Arc<App>,
        setup: Option<TikTokSetup>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(StdDuration::from_secs(30))
            .build()
            .unwrap_or_default();
        let clients = setup.map(|s| Clients {
            broker: s.broker_url.map(|u| BrokerClient::new(http.clone(), u)),
            oauth: TikTokOAuth::new(http.clone(), s.oauth, s.challenge),
            api: DisplayApi::new(http.clone(), s.api_base),
            loopback_port: s.loopback_port,
            poll_every: s.poll_every,
        });
        Arc::new(Self {
            app,
            clients,
            http,
            credentials,
            pending: Mutex::new(None),
            check_lock: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
        })
    }

    async fn enabled(&self) -> AppResult<bool> {
        Ok(self
            .app
            .store
            .settings()
            .get::<bool>(ENABLED_KEY)
            .await?
            .unwrap_or(false))
    }

    async fn source(&self) -> AppResult<Option<Source>> {
        Ok(self.app.store.settings().get(SOURCE_KEY).await?)
    }

    async fn save_source(&self, source: &Source) -> AppResult<()> {
        self.app.store.settings().set(SOURCE_KEY, source).await?;
        self.app.emit(AppEvent::TikTokChanged);
        Ok(())
    }

    fn clients(&self) -> AppResult<&Clients> {
        self.clients
            .as_ref()
            .ok_or_else(|| AppError::Invalid(REGISTER_NEXT_STEP.into()))
    }

    pub async fn status(&self) -> AppResult<TikTokStatus> {
        let Some(clients) = &self.clients else {
            return Ok(TikTokStatus::Unavailable {
                reason: "No TikTok developer app is configured for this installation.".into(),
                next_step: REGISTER_NEXT_STEP.into(),
            });
        };
        if let Some(source) = self.source().await? {
            if !self.enabled().await? {
                return Ok(TikTokStatus::Off);
            }
            if source.needs_reconnect {
                return Ok(TikTokStatus::Reconnect {
                    display_name: source.display_name,
                    message: source
                        .last_error
                        .unwrap_or_else(|| "TikTok needs you to sign in again.".into()),
                });
            }
            let pending_posts = self.app.store.tiktok_posts().pending().await?.len();
            return Ok(TikTokStatus::Connected {
                display_name: source.display_name,
                connected_at: source.connected_at,
                last_checked_at: source.last_checked_at,
                last_error: source.last_error,
                pending_posts,
            });
        }
        if self.load_app()?.is_none() {
            let Some(broker) = &clients.broker else {
                return Ok(TikTokStatus::Unavailable {
                    reason: "No TikTok developer app is set up yet.".into(),
                    next_step: REGISTER_NEXT_STEP.into(),
                });
            };
            match broker.status().await {
                Ok(s) if s.configured => {}
                Ok(_) => {
                    return Ok(TikTokStatus::Unavailable {
                        reason: "The OAuth broker has no TikTok developer app configured.".into(),
                        next_step: REGISTER_NEXT_STEP.into(),
                    });
                }
                Err(e) => {
                    return Ok(TikTokStatus::Unavailable {
                        reason: format!("The OAuth broker could not be reached ({e})."),
                        next_step: "Check that the OAuth broker is running and SHORTS_BROKER_URL \
                                    points at it, or paste your own TikTok app's key and secret."
                            .into(),
                    });
                }
            }
        }
        if !self.enabled().await? {
            return Ok(TikTokStatus::Off);
        }
        let connecting = self
            .pending
            .lock()
            .expect("pending lock")
            .as_ref()
            .is_some_and(|p| p.started.elapsed() < CONNECT_TIMEOUT);
        Ok(if connecting {
            TikTokStatus::Connecting
        } else {
            TikTokStatus::NotConnected
        })
    }

    /// The capability flag: new-post detection runs only while this is on.
    pub async fn set_enabled(&self, enabled: bool) -> AppResult<TikTokStatus> {
        if enabled {
            self.clients()?;
        }
        self.app.store.settings().set(ENABLED_KEY, &enabled).await?;
        self.app.emit(AppEvent::TikTokChanged);
        if enabled {
            self.wake.notify_one();
        }
        self.status().await
    }

    /// The owner's TikTok app, if pasted.
    fn load_app(&self) -> AppResult<Option<TikTokApp>> {
        let raw = self
            .credentials
            .get(APP_KEY)
            .map_err(|e| AppError::Intake(e.to_string()))?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    pub fn app_info(&self) -> AppResult<TikTokAppInfo> {
        let app = self.load_app()?;
        let port = self
            .clients
            .as_ref()
            .map_or(DEFAULT_LOOPBACK_PORT, |c| c.loopback_port);
        Ok(TikTokAppInfo {
            app_configured: app.is_some(),
            client_key: app.map(|a| a.client_key),
            broker_configured: self.clients.as_ref().is_some_and(|c| c.broker.is_some()),
            redirect_uri: format!("http://127.0.0.1:{port}{LOOPBACK_PATH}"),
        })
    }

    /// Saves the owner's TikTok app (client key and secret) in the credential store.
    pub async fn set_app(&self, client_key: &str, client_secret: &str) -> AppResult<TikTokStatus> {
        self.clients()?;
        let app = TikTokApp::new(client_key, client_secret).map_err(AppError::Invalid)?;
        let raw = serde_json::to_string(&app).expect("app serializes");
        self.credentials
            .set(APP_KEY, &raw)
            .map_err(|e| AppError::Intake(e.to_string()))?;
        info!("TikTok app saved");
        self.app.emit(AppEvent::TikTokChanged);
        self.status().await
    }

    pub async fn clear_app(&self) -> AppResult<TikTokStatus> {
        self.credentials
            .delete(APP_KEY)
            .map_err(|e| AppError::Intake(e.to_string()))?;
        self.app.emit(AppEvent::TikTokChanged);
        self.status().await
    }

    /// Starts the browser sign-in and returns the page to open: TikTok's consent page (direct,
    /// when the owner's app is saved) or the broker's.
    pub async fn begin_connect(self: &Arc<Self>) -> AppResult<Url> {
        let clients = self.clients()?;
        if !self.enabled().await? {
            return Err(AppError::Invalid(
                "turn on TikTok new-post detection first".into(),
            ));
        }
        let state = random_token();
        let verifier = random_token();
        let Some(tiktok_app) = self.load_app()? else {
            let Some(broker) = &clients.broker else {
                return Err(AppError::Invalid(REGISTER_NEXT_STEP.into()));
            };
            let url = broker.connect_url(&state, &challenge_for(&verifier));
            *self.pending.lock().expect("pending lock") = Some(PendingConnect {
                state,
                verifier,
                started: Instant::now(),
                direct: None,
                cancel: CancellationToken::new(),
            });
            self.app.emit(AppEvent::TikTokChanged);
            return Ok(url);
        };
        // A previous attempt (e.g. with other credentials) may still hold the port; stop it.
        drop(self.pending.lock().expect("pending lock").take());
        let loopback = Loopback::bind_port(clients.loopback_port, LOOPBACK_PATH)
            .await
            .map_err(|e| {
                AppError::Invalid(format!(
                    "could not listen for the TikTok sign-in on port {} ({e}); close whatever \
                     uses it and try again",
                    clients.loopback_port
                ))
            })?;
        let redirect = loopback.redirect_uri().to_owned();
        let url = clients
            .oauth
            .authorization_url(&tiktok_app, &redirect, &state, &verifier);
        let result = Arc::new(Mutex::new(None));
        let cancel = CancellationToken::new();
        *self.pending.lock().expect("pending lock") = Some(PendingConnect {
            state: state.clone(),
            verifier: verifier.clone(),
            started: Instant::now(),
            direct: Some(result.clone()),
            cancel: cancel.clone(),
        });
        self.app.emit(AppEvent::TikTokChanged);
        let this = self.clone();
        tokio::spawn(async move {
            let waited = tokio::select! {
                () = cancel.cancelled() => return,
                r = tokio::time::timeout(CONNECT_TIMEOUT, loopback.wait_for_code(&state)) => r,
            };
            let progress = match waited {
                Err(_) => ConnectProgress::Failed {
                    message: "The TikTok sign-in timed out; start again.".into(),
                },
                Ok(Err(e)) => ConnectProgress::Failed {
                    message: e.to_string(),
                },
                Ok(Ok(code)) => match this
                    .direct_exchange(&tiktok_app, &code, &redirect, &verifier)
                    .await
                {
                    Ok(status) => ConnectProgress::Connected { status },
                    Err(e) => ConnectProgress::Failed {
                        message: e.to_string(),
                    },
                },
            };
            *result.lock().expect("result lock") = Some(progress);
            this.app.emit(AppEvent::TikTokChanged);
        });
        Ok(url)
    }

    async fn direct_exchange(
        &self,
        tiktok_app: &TikTokApp,
        code: &str,
        redirect: &str,
        verifier: &str,
    ) -> AppResult<TikTokStatus> {
        let clients = self.clients()?;
        let tokens = clients
            .oauth
            .exchange_code(tiktok_app, code, redirect, verifier)
            .await
            .map_err(intake_error)?;
        self.complete(&tokens, false).await
    }

    /// Checks whether the browser sign-in finished; call it until it isn't `Pending`.
    pub async fn finish_connect(&self) -> AppResult<ConnectProgress> {
        let clients = self.clients()?;
        let (state, verifier, direct) = {
            let pending = self.pending.lock().expect("pending lock");
            match pending.as_ref() {
                Some(p) if p.started.elapsed() < CONNECT_TIMEOUT => {
                    (p.state.clone(), p.verifier.clone(), p.direct.clone())
                }
                _ => {
                    return Ok(ConnectProgress::Failed {
                        message: "Start connecting TikTok again.".into(),
                    });
                }
            }
        };
        if let Some(result) = direct {
            let done = result.lock().expect("result lock").clone();
            return Ok(match done {
                None => ConnectProgress::Pending,
                Some(progress) => {
                    self.pending.lock().expect("pending lock").take();
                    progress
                }
            });
        }
        let broker = clients
            .broker
            .as_ref()
            .ok_or_else(|| AppError::Invalid(REGISTER_NEXT_STEP.into()))?;
        let tokens = match broker.claim(&state, &verifier).await {
            Ok(Claim::Pending) => return Ok(ConnectProgress::Pending),
            Ok(Claim::Ready(tokens)) => tokens,
            Ok(Claim::Failed { message }) => {
                self.pending.lock().expect("pending lock").take();
                self.app.emit(AppEvent::TikTokChanged);
                return Ok(ConnectProgress::Failed { message });
            }
            Err(e) => return Err(intake_error(e)),
        };
        self.pending.lock().expect("pending lock").take();
        Ok(ConnectProgress::Connected {
            status: self.complete(&tokens, true).await?,
        })
    }

    /// Stores the tokens and the profile they belong to.
    async fn complete(&self, tokens: &TokenSet, via_broker: bool) -> AppResult<TikTokStatus> {
        let clients = self.clients()?;
        let user = clients
            .api
            .user_info(&tokens.access_token)
            .await
            .map_err(intake_error)?;
        let now = self.app.now();
        self.store_tokens(&StoredTokens::from_set(tokens, now))?;
        self.save_source(&Source {
            open_id: user.open_id,
            display_name: user.display_name,
            connected_at: now,
            since: now - Duration::days(FIRST_LOOKBACK_DAYS),
            last_checked_at: None,
            last_error: None,
            needs_reconnect: false,
            via_broker,
        })
        .await?;
        info!(via_broker, "TikTok connected");
        self.wake.notify_one();
        self.status().await
    }

    /// Forgets the connection (revoking the token on a best-effort basis). Detected posts stay
    /// in the library.
    pub async fn disconnect(&self) -> AppResult<TikTokStatus> {
        if let (Some(clients), Some(tokens)) = (&self.clients, self.load_tokens()?) {
            let via_broker = self.source().await?.is_none_or(|s| s.via_broker);
            let revoked = match (via_broker, &clients.broker, self.load_app()?) {
                (true, Some(broker), _) => broker.revoke(&tokens.access_token).await,
                (false, _, Some(app)) => clients.oauth.revoke(&app, &tokens.access_token).await,
                _ => Ok(()),
            };
            if let Err(e) = revoked {
                warn!(error = %e, "could not revoke the TikTok token");
            }
        }
        self.credentials
            .delete(CREDENTIAL_KEY)
            .map_err(|e| AppError::Intake(e.to_string()))?;
        self.app.store.settings().remove(SOURCE_KEY).await?;
        self.pending.lock().expect("pending lock").take();
        self.app.emit(AppEvent::TikTokChanged);
        self.status().await
    }

    fn load_tokens(&self) -> AppResult<Option<StoredTokens>> {
        let raw = self
            .credentials
            .get(CREDENTIAL_KEY)
            .map_err(|e| AppError::Intake(e.to_string()))?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    fn store_tokens(&self, tokens: &StoredTokens) -> AppResult<()> {
        let raw = serde_json::to_string(tokens).expect("tokens serialize");
        self.credentials
            .set(CREDENTIAL_KEY, &raw)
            .map_err(|e| AppError::Intake(e.to_string()))
    }

    async fn access_token(
        &self,
        clients: &Clients,
        via_broker: bool,
    ) -> Result<String, IntakeError> {
        let tokens =
            self.load_tokens()
                .ok()
                .flatten()
                .ok_or_else(|| IntakeError::Unauthorized {
                    message: "no stored TikTok sign-in".into(),
                })?;
        let now = self.app.now();
        if tokens.access_expires_at - now > Duration::seconds(REFRESH_MARGIN_SECS) {
            return Ok(tokens.access_token);
        }
        let fresh = if via_broker {
            let broker = clients
                .broker
                .as_ref()
                .ok_or_else(|| IntakeError::Unauthorized {
                    message: "this sign-in went through the OAuth broker, which isn't configured \
                          any more; connect again"
                        .into(),
                })?;
            broker.refresh(&tokens.refresh_token).await?
        } else {
            let app = self
                .load_app()
                .ok()
                .flatten()
                .ok_or_else(|| IntakeError::Unauthorized {
                    message: "the TikTok app's key and secret were removed; paste them and \
                              connect again"
                        .into(),
                })?;
            clients.oauth.refresh(&app, &tokens.refresh_token).await?
        };
        let stored = StoredTokens::from_set(&fresh, now);
        if let Err(e) = self.store_tokens(&stored) {
            warn!(error = %e, "could not store refreshed TikTok tokens");
        }
        Ok(stored.access_token)
    }

    /// Looks for new posts now. Each new post becomes an item awaiting its original file.
    pub async fn check_now(&self) -> AppResult<CheckReport> {
        let _guard = self.check_lock.lock().await;
        let clients = self.clients()?;
        if !self.enabled().await? {
            return Ok(CheckReport::default());
        }
        let Some(mut source) = self.source().await? else {
            return Ok(CheckReport::default());
        };
        if source.needs_reconnect {
            return Err(AppError::Intake("TikTok needs you to connect again".into()));
        }
        let result = self.fetch_new(clients, &source).await;
        source.last_checked_at = Some(self.app.now());
        match result {
            Ok(new_posts) => {
                source.last_error = None;
                self.save_source(&source).await?;
                // Workflows publish without anyone confirming matches, so unambiguous ones are
                // merged now; then items that waited for their caption go out.
                self.app.auto_match().await;
                self.app.apply_deferred_rules().await;
                let suggestions = self.app.match_suggestions().await?.len();
                if new_posts > 0 && suggestions > 0 {
                    self.app.emit(AppEvent::MatchSuggested { asset_id: None });
                }
                Ok(CheckReport {
                    new_posts,
                    suggestions,
                })
            }
            Err(e) => {
                if let IntakeError::Unauthorized { .. } = e {
                    source.needs_reconnect = true;
                }
                source.last_error = Some(e.to_string());
                self.save_source(&source).await?;
                Err(intake_error(e))
            }
        }
    }

    async fn fetch_new(&self, clients: &Clients, source: &Source) -> Result<usize, IntakeError> {
        let token = self.access_token(clients, source.via_broker).await?;
        let mut fresh = Vec::new();
        let mut cursor = None;
        'pages: for _ in 0..MAX_PAGES_PER_CHECK {
            let page = clients
                .api
                .list_videos(&token, cursor, MAX_PAGE_SIZE)
                .await?;
            let mut reached_known = false;
            for video in page.videos {
                if video.create_time < source.since.timestamp() {
                    break 'pages;
                }
                let known = self
                    .app
                    .store
                    .tiktok_posts()
                    .get(&video.id)
                    .await
                    .map_err(|e| IntakeError::Transient {
                        service: "the library",
                        message: e.to_string(),
                    })?
                    .is_some();
                if known {
                    reached_known = true;
                } else {
                    fresh.push(video);
                }
            }
            // Newest first: once a page holds a known post, older pages were seen before.
            if reached_known || !page.has_more || page.cursor.is_none() {
                break;
            }
            cursor = page.cursor;
        }
        // Oldest first, so library order follows posting order.
        fresh.sort_by_key(|v| v.create_time);
        let mut added = 0;
        for video in &fresh {
            match self
                .app
                .record_tiktok_post(video, &source.display_name)
                .await
            {
                Ok(Some(_)) => {
                    added += 1;
                    self.save_cover(video).await;
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, "could not add a TikTok post"),
            }
        }
        Ok(added)
    }

    /// Copies the cover image into the cache (the TikTok link expires). Best effort.
    async fn save_cover(&self, video: &shorts_adapters::tiktok::TikTokVideo) {
        let Some(url) = &video.cover_image_url else {
            return;
        };
        let (bytes, ext) = match fetch_cover(&self.http, url, COVER_MAX_BYTES).await {
            Ok(cover) => cover,
            Err(e) => {
                warn!(error = %e, "could not fetch a TikTok cover");
                return;
            }
        };
        let safe_id: String = video
            .id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let dir = self.app.paths.cache.join("covers");
        let path = dir.join(format!("tiktok-{safe_id}.{ext}"));
        let written = async {
            tokio::fs::create_dir_all(&dir).await?;
            tokio::fs::write(&path, &bytes).await
        }
        .await;
        if let Err(e) = written {
            warn!(error = %e, "could not save a TikTok cover");
            return;
        }
        if let Err(e) = self
            .app
            .store
            .tiktok_posts()
            .set_cover_path(&video.id, &path.to_string_lossy())
            .await
        {
            warn!(error = %e, "could not record a TikTok cover");
        }
        self.app.emit(AppEvent::LibraryChanged);
    }

    /// Checks for new posts on a timer (and when woken) until `shutdown`.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let Some(every) = self.clients.as_ref().map(|c| c.poll_every) else {
            return;
        };
        loop {
            let ready = matches!(self.status().await, Ok(TikTokStatus::Connected { .. }));
            if ready && let Err(e) = self.check_now().await {
                warn!(error = %e, "TikTok check failed");
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(every) => {}
                () = self.wake.notified() => {}
            }
        }
    }
}
