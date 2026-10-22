//! YouTube channel connections: the owner's Google OAuth client, browser sign-in through the
//! loopback redirect, access tokens for the adapter, quota spend, and the per-channel
//! private-uploads setting.
//!
//! Secrets live in the OS credential store only: the OAuth client (`google_oauth_client`) and
//! each channel's grant (`youtube:<account id>`, with the refresh token and channel id). SQLite
//! holds the account, its capability record and a small status record per channel; views
//! never include secrets, tokens or the channel id. Access tokens are kept in memory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rand::RngCore;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use shorts_adapters::broker::{base64url, challenge_for};
use shorts_adapters::google::{ClientConfig, GoogleEndpoints, Loopback, OAuthClient, OAuthError};
use shorts_adapters::youtube::{
    API_BASE, AccessTokens, QuotaLedger, QuotaLimits, QuotaReport, YouTubeAdapter, YouTubeConfig,
    quota_report,
};
use shorts_adapters::{Adapter, AdapterError};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, ExecutionOwner, NextAction, Platform,
    PublicationEvent, PublicationState, PublishAccess,
};
use shorts_store::{PublicationFilter, Store};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::app::App;
use crate::credentials::CredentialStore;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::views::AccountView;
use shorts_cloud::{CloudCredential, Secret};

const CLIENT_KEY: &str = "google_oauth_client";
const CHANNELS_KEY: &str = "youtube_channels";
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(10 * 60);
/// Refresh the access token when it has less than this left.
const REFRESH_MARGIN: Duration = Duration::seconds(120);

pub const CLIENT_NEXT_STEP: &str = "Create a Google Cloud project with the YouTube Data API v3 \
    enabled and a Desktop app OAuth client, then paste its client ID and secret here (see \
    docs/human-scope.md).";

fn grant_key(account: AccountId) -> String {
    format!("youtube:{account}")
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64url(&bytes)
}

fn credential_error(e: impl std::fmt::Display) -> AppError {
    AppError::Intake(format!("credential store: {e}"))
}

/// Where the Google and YouTube endpoints are and how much quota the project has.
#[derive(Debug, Clone)]
pub struct YouTubeSetup {
    pub google: GoogleEndpoints,
    pub api_base: Url,
    pub limits: QuotaLimits,
    /// Per-request timeout. Uploads send one chunk per request.
    pub request_timeout: StdDuration,
    pub chunk_bytes: u64,
    pub poll_every: StdDuration,
}

impl Default for YouTubeSetup {
    fn default() -> Self {
        let config = YouTubeConfig::default();
        Self {
            google: GoogleEndpoints::default(),
            api_base: Url::parse(API_BASE).expect("valid URL"),
            limits: QuotaLimits::default(),
            // Long enough for one chunk on a slow uplink (8 MiB at ~0.25 Mbit/s).
            request_timeout: StdDuration::from_secs(300),
            chunk_bytes: config.chunk_bytes,
            poll_every: config.poll_every,
        }
    }
}

impl YouTubeSetup {
    /// Google's defaults, with the daily quotas raised by `SHORTS_YOUTUBE_UPLOADS_PER_DAY` and
    /// `SHORTS_YOUTUBE_UNITS_PER_DAY` when Google granted more.
    pub fn from_env() -> Self {
        let mut setup = Self::default();
        let num = |key: &str| {
            std::env::var(key)
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|n| *n > 0)
        };
        if let Some(n) = num("SHORTS_YOUTUBE_UPLOADS_PER_DAY") {
            setup.limits.uploads_per_day = n;
        }
        if let Some(n) = num("SHORTS_YOUTUBE_UNITS_PER_DAY") {
            setup.limits.units_per_day = n;
        }
        setup
    }
}

/// A channel's grant, kept in the credential store.
#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    refresh_token: String,
    channel_id: String,
    scope: String,
    granted_at: DateTime<Utc>,
    /// When Google said the refresh token expires (7 days while the consent screen is in
    /// Testing); `None` when it didn't say.
    expires_at: Option<DateTime<Utc>>,
}

/// Non-secret per-channel status, kept in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ChannelState {
    connected_at: DateTime<Utc>,
    sign_in_expires_at: Option<DateTime<Utc>>,
    needs_reconnect: bool,
    last_error: Option<String>,
}

/// One YouTube destination as the Settings screen shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YouTubeChannel {
    pub account_id: String,
    pub display_name: String,
    /// Signed in, so the app can upload.
    pub connected: bool,
    pub needs_reconnect: bool,
    pub last_error: Option<String>,
    pub private_only: bool,
    pub notes: Vec<String>,
    pub connected_at: Option<DateTime<Utc>>,
    /// When the sign-in runs out (7 days after connecting while the consent screen is in
    /// Testing).
    pub sign_in_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YouTubeStatus {
    /// The owner's OAuth client ID and secret are saved.
    pub client_configured: bool,
    pub next_step: Option<String>,
    pub connecting: bool,
    pub channels: Vec<YouTubeChannel>,
    pub quota: QuotaReport,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum YouTubeConnectProgress {
    /// No sign-in is running.
    Idle,
    Pending,
    Connected {
        account: AccountView,
    },
    Failed {
        message: String,
    },
}

/// Quota spend recorded in SQLite.
pub(crate) struct StoreQuota(pub(crate) Store);

#[async_trait]
impl QuotaLedger for StoreQuota {
    async fn used(&self, bucket: &str, day: NaiveDate) -> Result<u32, String> {
        self.0
            .quota()
            .used(bucket, day)
            .await
            .map_err(|e| e.to_string())
    }

    async fn spend(&self, bucket: &str, day: NaiveDate, units: u32) -> Result<u32, String> {
        self.0
            .quota()
            .spend(bucket, day, units)
            .await
            .map_err(|e| e.to_string())
    }

    async fn exhaust(&self, bucket: &str, day: NaiveDate, limit: u32) -> Result<(), String> {
        self.0
            .quota()
            .exhaust(bucket, day, limit)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Access tokens from each channel's refresh token, cached in memory.
pub(crate) struct GoogleTokenSource {
    oauth: OAuthClient,
    credentials: Arc<dyn CredentialStore>,
    store: Store,
    clock: Arc<dyn shorts_scheduler::Clock>,
    cache: Mutex<HashMap<AccountId, (String, DateTime<Utc>)>>,
    refresh: tokio::sync::Mutex<()>,
}

impl GoogleTokenSource {
    fn client(&self) -> Result<Option<ClientConfig>, String> {
        let raw = self
            .credentials
            .get(CLIENT_KEY)
            .map_err(|e| e.to_string())?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    fn grant(&self, account: AccountId) -> Result<Option<Grant>, String> {
        let raw = self
            .credentials
            .get(&grant_key(account))
            .map_err(|e| e.to_string())?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    fn save_grant(&self, account: AccountId, grant: &Grant) -> Result<(), String> {
        let raw = serde_json::to_string(grant).expect("grant serializes");
        self.credentials
            .set(&grant_key(account), &raw)
            .map_err(|e| e.to_string())
    }

    fn cached(&self, account: AccountId) -> Option<String> {
        let now = self.clock.now();
        self.cache
            .lock()
            .expect("token cache lock")
            .get(&account)
            .filter(|(_, expires)| *expires - now > REFRESH_MARGIN)
            .map(|(t, _)| t.clone())
    }

    fn prime(&self, account: AccountId, token: &str, expires_in: i64) {
        let expires = self.clock.now() + Duration::seconds(expires_in.max(0));
        self.cache
            .lock()
            .expect("token cache lock")
            .insert(account, (token.to_owned(), expires));
    }

    fn forget(&self, account: AccountId) {
        self.cache
            .lock()
            .expect("token cache lock")
            .remove(&account);
    }

    async fn mark_reconnect(&self, account: AccountId, message: &str) {
        let result = async {
            let mut channels: HashMap<AccountId, ChannelState> = self
                .store
                .settings()
                .get(CHANNELS_KEY)
                .await?
                .unwrap_or_default();
            if let Some(state) = channels.get_mut(&account) {
                state.needs_reconnect = true;
                state.last_error = Some(message.to_owned());
                self.store.settings().set(CHANNELS_KEY, &channels).await?;
            }
            Ok::<_, shorts_store::StoreError>(())
        }
        .await;
        if let Err(e) = result {
            warn!(error = %e, "could not record that a channel needs reconnecting");
        }
    }
}

#[async_trait]
impl AccessTokens for GoogleTokenSource {
    async fn access_token(&self, account: AccountId) -> Result<String, AdapterError> {
        if let Some(token) = self.cached(account) {
            return Ok(token);
        }
        let _guard = self.refresh.lock().await;
        if let Some(token) = self.cached(account) {
            return Ok(token);
        }
        let store_err = |e: String| AdapterError::Transient {
            message: format!("credential store: {e}"),
            retry_after: None,
        };
        let Some(client) = self.client().map_err(store_err)? else {
            return Err(AdapterError::Authentication {
                message: "enter the Google OAuth client ID and secret in Settings".to_owned(),
            });
        };
        let Some(mut grant) = self.grant(account).map_err(store_err)? else {
            return Err(AdapterError::Authentication {
                message: "the channel is not signed in; connect it in Settings".to_owned(),
            });
        };
        match self.oauth.refresh(&client, &grant.refresh_token).await {
            Ok(tokens) => {
                self.prime(account, &tokens.access_token, tokens.expires_in);
                if let Some(rotated) = tokens.refresh_token {
                    grant.refresh_token = rotated;
                    if let Err(e) = self.save_grant(account, &grant) {
                        warn!(error = %e, "could not store a rotated Google refresh token");
                    }
                }
                Ok(tokens.access_token)
            }
            Err(OAuthError::Reauthorize { message }) => {
                let message = format!(
                    "Google ended the channel's sign-in ({message}); reconnect it in Settings. \
                     While the consent screen is in Testing this happens every 7 days."
                );
                self.mark_reconnect(account, &message).await;
                Err(AdapterError::Authentication { message })
            }
            Err(OAuthError::BadClient { message }) => {
                let message = format!(
                    "Google rejected the OAuth client ID or secret ({message}); check them in \
                     Settings"
                );
                self.mark_reconnect(account, &message).await;
                Err(AdapterError::Authentication { message })
            }
            Err(e) => Err(AdapterError::Transient {
                message: e.to_string(),
                retry_after: None,
            }),
        }
    }

    async fn invalidate(&self, account: AccountId, token: &str) {
        let mut cache = self.cache.lock().expect("token cache lock");
        if cache.get(&account).is_some_and(|(t, _)| t == token) {
            cache.remove(&account);
        }
    }
}

struct PendingConnect {
    id: u64,
    progress: YouTubeConnectProgress,
    cancel: CancellationToken,
}

/// The YouTube connector.
pub struct YouTubeConnector {
    app: Arc<App>,
    oauth: OAuthClient,
    adapter: Arc<YouTubeAdapter>,
    tokens: Arc<GoogleTokenSource>,
    credentials: Arc<dyn CredentialStore>,
    pending: Mutex<Option<PendingConnect>>,
    next_id: Mutex<u64>,
}

impl YouTubeConnector {
    /// Builds the connector and registers its adapter with the app.
    pub fn new(
        app: Arc<App>,
        setup: YouTubeSetup,
        credentials: Arc<dyn CredentialStore>,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(setup.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        let oauth = OAuthClient::new(http.clone(), setup.google.clone());
        let clock = app.clock();
        let tokens = Arc::new(GoogleTokenSource {
            oauth: oauth.clone(),
            credentials: credentials.clone(),
            store: app.store.clone(),
            clock: clock.clone(),
            cache: Mutex::new(HashMap::new()),
            refresh: tokio::sync::Mutex::new(()),
        });
        let adapter = Arc::new(YouTubeAdapter::new(
            http,
            YouTubeConfig {
                api_base: setup.api_base,
                chunk_bytes: setup.chunk_bytes,
                limits: setup.limits,
                poll_every: setup.poll_every,
            },
            tokens.clone(),
            Arc::new(StoreQuota(app.store.clone())),
            Arc::new(move || clock.now()),
        ));
        app.register_adapter(adapter.clone());
        Arc::new(Self {
            app,
            oauth,
            adapter,
            tokens,
            credentials,
            pending: Mutex::new(None),
            next_id: Mutex::new(0),
        })
    }

    async fn channels_state(&self) -> AppResult<HashMap<AccountId, ChannelState>> {
        Ok(self
            .app
            .store
            .settings()
            .get(CHANNELS_KEY)
            .await?
            .unwrap_or_default())
    }

    async fn save_channels_state(&self, state: &HashMap<AccountId, ChannelState>) -> AppResult<()> {
        self.app.store.settings().set(CHANNELS_KEY, state).await?;
        Ok(())
    }

    fn client(&self) -> AppResult<Option<ClientConfig>> {
        self.tokens.client().map_err(credential_error)
    }

    pub async fn status(&self) -> AppResult<YouTubeStatus> {
        let client_configured = self.client()?.is_some();
        let states = self.channels_state().await?;
        let mut channels = Vec::new();
        for account in self.app.store.accounts().list().await? {
            if account.platform != Platform::YoutubeShorts {
                continue;
            }
            let state = states.get(&account.id);
            let connected = account.credential_ref.is_some()
                && self
                    .tokens
                    .grant(account.id)
                    .map_err(credential_error)?
                    .is_some();
            channels.push(YouTubeChannel {
                account_id: account.id.to_string(),
                display_name: account.display_name.clone(),
                connected,
                needs_reconnect: connected && state.is_some_and(|s| s.needs_reconnect),
                last_error: state.and_then(|s| s.last_error.clone()),
                private_only: account.capabilities.private_only,
                notes: account.capabilities.notes.clone(),
                connected_at: state.map(|s| s.connected_at),
                sign_in_expires_at: state.and_then(|s| s.sign_in_expires_at),
            });
        }
        let connecting = matches!(
            self.pending.lock().expect("pending lock").as_ref(),
            Some(PendingConnect {
                progress: YouTubeConnectProgress::Pending,
                ..
            })
        );
        let quota = quota_report(
            self.adapter.quota_ledger().as_ref(),
            self.adapter.limits(),
            self.app.now(),
        )
        .await
        .map_err(AppError::Intake)?;
        Ok(YouTubeStatus {
            client_configured,
            next_step: (!client_configured).then(|| CLIENT_NEXT_STEP.to_owned()),
            connecting,
            channels,
            quota,
        })
    }

    /// Saves the owner's Desktop app OAuth client in the credential store.
    pub async fn set_client(
        &self,
        client_id: &str,
        client_secret: &str,
    ) -> AppResult<YouTubeStatus> {
        let client = ClientConfig::new(client_id, client_secret).map_err(AppError::Invalid)?;
        let raw = serde_json::to_string(&client).expect("client serializes");
        self.credentials
            .set(CLIENT_KEY, &raw)
            .map_err(credential_error)?;
        info!("Google OAuth client saved");
        self.app.emit(AppEvent::YouTubeChanged);
        self.status().await
    }

    pub async fn clear_client(&self) -> AppResult<YouTubeStatus> {
        self.credentials
            .delete(CLIENT_KEY)
            .map_err(credential_error)?;
        self.app.emit(AppEvent::YouTubeChanged);
        self.status().await
    }

    /// Starts the browser sign-in and returns Google's consent page to open. With `account`,
    /// reconnects (or upgrades an export-package account to) that destination.
    pub async fn begin_connect(self: &Arc<Self>, account: Option<AccountId>) -> AppResult<Url> {
        let client = self
            .client()?
            .ok_or_else(|| AppError::Invalid(CLIENT_NEXT_STEP.into()))?;
        if let Some(id) = account {
            let a = self.app.store.accounts().require(id).await?;
            if a.platform != Platform::YoutubeShorts {
                return Err(AppError::Invalid("that is not a YouTube account".into()));
            }
        }
        let loopback = Loopback::bind().await?;
        let state = random_token();
        let verifier = random_token();
        let redirect_uri = loopback.redirect_uri().to_owned();
        let url =
            self.oauth
                .authorization_url(&client, &redirect_uri, &state, &challenge_for(&verifier));
        let cancel = CancellationToken::new();
        let id = {
            let mut next = self.next_id.lock().expect("id lock");
            *next += 1;
            *next
        };
        if let Some(previous) = self
            .pending
            .lock()
            .expect("pending lock")
            .replace(PendingConnect {
                id,
                progress: YouTubeConnectProgress::Pending,
                cancel: cancel.clone(),
            })
        {
            previous.cancel.cancel();
        }
        let this = self.clone();
        tokio::spawn(async move {
            let outcome = tokio::select! {
                () = cancel.cancelled() => return,
                r = tokio::time::timeout(CONNECT_TIMEOUT, loopback.wait_for_code(&state)) => r,
            };
            let progress = match outcome {
                Err(_) => YouTubeConnectProgress::Failed {
                    message: "The sign-in timed out. Start connecting again.".into(),
                },
                Ok(Err(e)) => YouTubeConnectProgress::Failed {
                    message: e.to_string(),
                },
                Ok(Ok(code)) => match this
                    .complete(&client, &code, &verifier, &redirect_uri, account)
                    .await
                {
                    Ok(view) => YouTubeConnectProgress::Connected { account: view },
                    Err(e) => YouTubeConnectProgress::Failed {
                        message: e.to_string(),
                    },
                },
            };
            if let YouTubeConnectProgress::Failed { message } = &progress {
                warn!(%message, "YouTube sign-in did not complete");
            }
            let mut pending = this.pending.lock().expect("pending lock");
            if let Some(p) = pending.as_mut().filter(|p| p.id == id) {
                p.progress = progress;
            }
            drop(pending);
            this.app.emit(AppEvent::YouTubeChanged);
        });
        self.app.emit(AppEvent::YouTubeChanged);
        Ok(url)
    }

    /// Where the browser sign-in stands; call it until it isn't `pending`.
    pub fn connect_progress(&self) -> YouTubeConnectProgress {
        let mut pending = self.pending.lock().expect("pending lock");
        match pending.as_ref().map(|p| p.progress.clone()) {
            None => YouTubeConnectProgress::Idle,
            Some(YouTubeConnectProgress::Pending) => YouTubeConnectProgress::Pending,
            Some(done) => {
                pending.take();
                done
            }
        }
    }

    /// Stops waiting for a browser sign-in.
    pub fn cancel_connect(&self) {
        if let Some(p) = self.pending.lock().expect("pending lock").take() {
            p.cancel.cancel();
        }
        self.app.emit(AppEvent::YouTubeChanged);
    }

    async fn complete(
        &self,
        client: &ClientConfig,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        reconnect: Option<AccountId>,
    ) -> AppResult<AccountView> {
        let tokens = self
            .oauth
            .exchange_code(client, code, verifier, redirect_uri)
            .await
            .map_err(|e| AppError::Intake(e.to_string()))?;
        let missing = tokens.missing_scopes();
        if !missing.is_empty() {
            return Err(AppError::Invalid(format!(
                "Google didn't grant {}; connect again and allow uploading and viewing your \
                 YouTube videos",
                missing.join(" and ")
            )));
        }
        let Some(refresh_token) = tokens.refresh_token.clone() else {
            return Err(AppError::Invalid(
                "Google didn't return a long-lived sign-in. Remove the app's access in your \
                 Google account's security settings (Third-party connections) and connect again."
                    .into(),
            ));
        };

        // Find out which channel this is, using the new token under a provisional id.
        let probe = reconnect.unwrap_or_default();
        self.tokens
            .prime(probe, &tokens.access_token, tokens.expires_in);
        let channel = match self.adapter.my_channel(probe).await {
            Ok(c) => c,
            Err(e) => {
                self.tokens.forget(probe);
                return Err(AppError::Adapter(e));
            }
        };

        // Reuse the destination this channel already has, if any.
        let accounts = self.app.store.accounts().list().await?;
        let mut existing = None;
        for a in accounts
            .iter()
            .filter(|a| a.platform == Platform::YoutubeShorts)
        {
            let grant = self.tokens.grant(a.id).map_err(credential_error)?;
            if grant.is_some_and(|g| g.channel_id == channel.id) {
                existing = Some(a.clone());
                break;
            }
        }
        let target = match (reconnect, existing) {
            (Some(id), Some(found)) if found.id != id => {
                self.tokens.forget(probe);
                return Err(AppError::Invalid(format!(
                    "You signed in to {}, which is already connected as \"{}\". Reconnect that \
                     account instead.",
                    channel.title, found.display_name
                )));
            }
            (Some(id), _) => {
                let account = self.app.store.accounts().require(id).await?;
                let previous = self.tokens.grant(id).map_err(credential_error)?;
                if previous.is_some_and(|g| g.channel_id != channel.id) {
                    self.tokens.forget(probe);
                    return Err(AppError::Invalid(format!(
                        "You signed in to {}, a different channel from the one connected as \
                         \"{}\". Sign in with that channel, or add the new one separately.",
                        channel.title, account.display_name
                    )));
                }
                Some(account)
            }
            (None, found) => found,
        };

        let now = self.app.now();
        let mut account = match target {
            Some(a) => a,
            None => {
                let mut a = ConnectedAccount::manual(Platform::YoutubeShorts, &channel.title, now);
                a.id = probe;
                // Until the owner says the project passed the audit.
                a.capabilities.private_only = true;
                a
            }
        };
        let is_new = self.app.store.accounts().get(account.id).await?.is_none();
        let export_only = matches!(
            &account.capabilities.publish_access,
            PublishAccess::ManualOnly { missing } if missing.next_action == NextAction::ExportPackage
        );
        if export_only {
            // An export-package destination becoming an API one starts private-only too. (A
            // disconnected channel keeps the owner's earlier choice.)
            account.capabilities.private_only = true;
        }
        let grant = Grant {
            refresh_token,
            channel_id: channel.id.clone(),
            scope: tokens.scope.clone(),
            granted_at: now,
            expires_at: tokens
                .refresh_token_expires_in
                .map(|s| now + Duration::seconds(s)),
        };
        self.tokens
            .save_grant(account.id, &grant)
            .map_err(credential_error)?;
        if probe != account.id {
            self.tokens.forget(probe);
        }
        self.tokens
            .prime(account.id, &tokens.access_token, tokens.expires_in);
        account.credential_ref = Some(grant_key(account.id));
        let private_only = account.capabilities.private_only;
        account.capabilities = self.adapter.capabilities(&account).await?;
        account.capabilities.private_only = private_only;
        account.updated_at = now;
        if is_new {
            self.app.store.accounts().insert(&account).await?;
        } else {
            self.app.store.accounts().update(&account).await?;
        }

        let mut states = self.channels_state().await?;
        states.insert(
            account.id,
            ChannelState {
                connected_at: now,
                sign_in_expires_at: grant.expires_at,
                needs_reconnect: false,
                last_error: None,
            },
        );
        self.save_channels_state(&states).await?;
        info!(account = %account.id, "YouTube channel connected");
        self.resume_waiting(account.id).await?;
        Ok(AccountView::from(&account))
    }

    /// Publications that stopped for want of a sign-in continue.
    async fn resume_waiting(&self, account: AccountId) -> AppResult<()> {
        let waiting = self
            .app
            .store
            .publications()
            .list(&PublicationFilter {
                account_id: Some(account),
                states: vec![PublicationState::NeedsAuthentication],
                // The cloud runner resumes its own once it gets the new credential.
                owner: Some(ExecutionOwner::Desktop),
                ..Default::default()
            })
            .await?;
        for p in waiting {
            if let Err(e) = self
                .app
                .transition(p.id(), |_| PublicationEvent::Reauthenticated)
                .await
            {
                warn!(publication = %p.id(), error = %e, "could not resume after reconnecting");
            }
        }
        Ok(())
    }

    /// Signs the channel out (revoking the grant with Google when possible). The destination
    /// stays, using export packages until it is connected again.
    pub async fn disconnect(&self, account_id: AccountId) -> AppResult<YouTubeStatus> {
        let mut account = self.app.store.accounts().require(account_id).await?;
        if account.platform != Platform::YoutubeShorts {
            return Err(AppError::Invalid("that is not a YouTube account".into()));
        }
        if let Some(grant) = self.tokens.grant(account_id).map_err(credential_error)?
            && let Err(e) = self.oauth.revoke(&grant.refresh_token).await
        {
            warn!(error = %e, "could not revoke the Google grant");
        }
        self.credentials
            .delete(&grant_key(account_id))
            .map_err(credential_error)?;
        self.tokens.forget(account_id);
        let private_only = account.capabilities.private_only;
        account.capabilities = Capabilities::reconnect_required(
            "Disconnected. Connect the channel again to upload automatically; until then its \
             publications use export packages.",
            self.app.now(),
        );
        account.capabilities.private_only = private_only;
        account.credential_ref = None;
        account.updated_at = self.app.now();
        self.app.store.accounts().update(&account).await?;
        let mut states = self.channels_state().await?;
        states.remove(&account_id);
        self.save_channels_state(&states).await?;
        self.app.emit(AppEvent::YouTubeChanged);
        self.status().await
    }

    /// Public uploads, for when the Google Cloud project has passed YouTube's audit; private
    /// otherwise.
    pub async fn set_public_uploads(
        &self,
        account_id: AccountId,
        public: bool,
    ) -> AppResult<YouTubeStatus> {
        let mut account = self.app.store.accounts().require(account_id).await?;
        if account.platform != Platform::YoutubeShorts {
            return Err(AppError::Invalid("that is not a YouTube account".into()));
        }
        account.capabilities.private_only = !public;
        if account.credential_ref.is_some() {
            account.capabilities = self.adapter.capabilities(&account).await?;
            account.capabilities.private_only = !public;
        }
        account.updated_at = self.app.now();
        self.app.store.accounts().update(&account).await?;
        self.app.emit(AppEvent::YouTubeChanged);
        self.status().await
    }
}

// --- cloud runner ----------------------------------------------------------------------------

/// The channel's credential for the cloud runner: the OAuth client and the refresh token.
pub(crate) fn cloud_credential(
    credentials: &dyn CredentialStore,
    account: AccountId,
) -> Result<Option<CloudCredential>, String> {
    let read = |key: &str| credentials.get(key).map_err(|e| e.to_string());
    let client: Option<ClientConfig> =
        read(CLIENT_KEY)?.and_then(|r| serde_json::from_str(&r).ok());
    let grant: Option<Grant> =
        read(&grant_key(account))?.and_then(|r| serde_json::from_str(&r).ok());
    Ok(match (client, grant) {
        (Some(client), Some(grant)) => Some(CloudCredential::YoutubeShorts {
            client_id: client.client_id,
            client_secret: Secret::new(client.client_secret),
            refresh_token: Secret::new(grant.refresh_token),
            channel_id: grant.channel_id,
        }),
        _ => None,
    })
}

/// Keeps a refresh token the cloud runner was given by Google in place of the old one.
pub(crate) fn import_cloud_credential(
    credentials: &dyn CredentialStore,
    account: AccountId,
    credential: &CloudCredential,
) -> Result<(), String> {
    let CloudCredential::YoutubeShorts {
        refresh_token,
        channel_id,
        ..
    } = credential
    else {
        return Err("not a YouTube credential".into());
    };
    let key = grant_key(account);
    let Some(mut grant) = credentials
        .get(&key)
        .map_err(|e| e.to_string())?
        .and_then(|r| serde_json::from_str::<Grant>(&r).ok())
    else {
        return Ok(());
    };
    if &grant.channel_id != channel_id {
        return Err("the cloud runner's credential is for another channel".into());
    }
    grant.refresh_token = refresh_token.expose().to_owned();
    let raw = serde_json::to_string(&grant).expect("grant serializes");
    credentials.set(&key, &raw).map_err(|e| e.to_string())
}
