//! Snapchat public profile connections: the owner's Snap OAuth app (client id, secret and the
//! redirect address registered for it), a browser sign-in, hourly access tokens for the
//! Spotlight adapter.
//!
//! Two ways back from the browser, chosen by the registered redirect address:
//! - a loopback address (`http://127.0.0.1:<port>/…` or `http://localhost:<port>/…`): the app
//!   listens on that port and picks the code up itself;
//! - any https page (for example the project's `snap-callback.html` on GitHub Pages, if Snap
//!   doesn't accept a loopback address): the page shows a short text the owner pastes back into
//!   the app.
//!
//! Secrets live in the OS credential store only: the OAuth app (`snap_oauth_client`) and each
//! profile's grant (`snapchat:<account id>`, refresh token and profile id). Access tokens are
//! kept in memory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use shorts_adapters::broker::base64url;
use shorts_adapters::google::Loopback;
use shorts_adapters::snapchat::{
    API_BASE, SnapAdapter, SnapAuth, SnapAuthError, SnapClient, SnapConfig, SnapEndpoints,
    SnapOAuth, SnapSession,
};
use shorts_adapters::{Adapter, AdapterError};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, ExecutionOwner, Platform, PublicationEvent,
    PublicationState, snapchat,
};
use shorts_store::{PublicationFilter, Store};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::app::App;
use crate::credentials::CredentialStore;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::views::AccountView;

const CLIENT_KEY: &str = "snap_oauth_client";
const PROFILES_KEY: &str = "snapchat_profiles";
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(10 * 60);
const REFRESH_MARGIN: Duration = Duration::seconds(120);
pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:47615/snapchat/callback";

pub const CLIENT_NEXT_STEP: &str = "Create an OAuth app in Snapchat Ads Manager (Business \
    Details → OAuth apps) with the redirect address shown here, ask Snap to allowlist it for \
    the Public Profile API, then paste its client id and secret (see docs/human-scope.md).";

fn grant_key(account: AccountId) -> String {
    format!("snapchat:{account}")
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64url(&bytes)
}

fn credential_error(e: impl std::fmt::Display) -> AppError {
    AppError::Intake(format!("credential store: {e}"))
}

/// Where Snap's endpoints are.
#[derive(Debug, Clone)]
pub struct SnapSetup {
    pub endpoints: SnapEndpoints,
    pub api_base: Url,
    pub locale: String,
    pub request_timeout: StdDuration,
    pub part_timeout: StdDuration,
}

impl Default for SnapSetup {
    fn default() -> Self {
        Self {
            endpoints: SnapEndpoints::default(),
            api_base: Url::parse(API_BASE).expect("valid URL"),
            locale: snapchat::DEFAULT_LOCALE.to_owned(),
            request_timeout: StdDuration::from_secs(60),
            part_timeout: SnapConfig::default().part_timeout,
        }
    }
}

impl SnapSetup {
    /// The defaults, with the Spotlight locale from `SHORTS_SNAPCHAT_LOCALE` (e.g. `en_GB`).
    pub fn from_env() -> Self {
        let mut setup = Self::default();
        if let Ok(v) = std::env::var("SHORTS_SNAPCHAT_LOCALE") {
            let v = v.trim();
            if (2..=10).contains(&v.len()) && v.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
            {
                setup.locale = v.to_owned();
            }
        }
        setup
    }
}

/// The owner's Snap OAuth app and its registered redirect address.
#[derive(Clone, Serialize, Deserialize)]
struct AppConfig {
    client: SnapClient,
    redirect_uri: String,
}

/// How the browser sign-in comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReturnMode {
    /// The app listens on the loopback port.
    Loopback,
    /// The owner pastes the text the redirect page shows.
    Paste,
}

fn return_mode(redirect: &Url) -> ReturnMode {
    let local = matches!(redirect.host_str(), Some("127.0.0.1" | "localhost"));
    if redirect.scheme() == "http" && local && redirect.port().is_some() {
        ReturnMode::Loopback
    } else {
        ReturnMode::Paste
    }
}

fn parse_redirect(raw: &str) -> AppResult<Url> {
    let url = Url::parse(raw.trim())
        .map_err(|_| AppError::Invalid("the redirect address isn't a valid URL".into()))?;
    match (url.scheme(), return_mode(&url)) {
        ("https", _) | (_, ReturnMode::Loopback) => Ok(url),
        _ => Err(AppError::Invalid(
            "use https://… or http://127.0.0.1:<port>/… as the redirect address".into(),
        )),
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    refresh_token: String,
    profile_id: String,
    granted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProfileState {
    connected_at: DateTime<Utc>,
    needs_reconnect: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapProfileView {
    pub account_id: String,
    pub display_name: String,
    pub connected: bool,
    pub needs_reconnect: bool,
    pub last_error: Option<String>,
    pub notes: Vec<String>,
    pub connected_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapStatus {
    pub client_configured: bool,
    /// The redirect address saved with the OAuth app (not secret), or the suggested default.
    pub redirect_uri: String,
    pub return_mode: ReturnMode,
    pub next_step: Option<String>,
    /// A loopback sign-in is waiting for the browser.
    pub connecting: bool,
    /// A sign-in is waiting for the owner to paste the code page's text.
    pub awaiting_code: bool,
    pub profiles: Vec<SnapProfileView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SnapConnectProgress {
    Idle,
    Pending,
    Connected { account: AccountView },
    Failed { message: String },
}

/// An access token, the profile it is for, and when it expires.
type CachedToken = (String, String, DateTime<Utc>);

/// Access tokens from each profile's refresh token, cached in memory.
pub(crate) struct SnapTokenSource {
    oauth: SnapOAuth,
    credentials: Arc<dyn CredentialStore>,
    store: Store,
    clock: Arc<dyn shorts_scheduler::Clock>,
    cache: Mutex<HashMap<AccountId, CachedToken>>,
    refresh: tokio::sync::Mutex<()>,
}

impl SnapTokenSource {
    fn app_config(&self) -> Result<Option<AppConfig>, String> {
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

    fn cached(&self, account: AccountId) -> Option<SnapSession> {
        let now = self.clock.now();
        self.cache
            .lock()
            .expect("token cache lock")
            .get(&account)
            .filter(|(_, _, expires)| *expires - now > REFRESH_MARGIN)
            .map(|(token, profile, _)| SnapSession {
                access_token: token.clone(),
                profile_id: profile.clone(),
            })
    }

    fn prime(&self, account: AccountId, token: &str, profile: &str, expires_in: i64) {
        let expires = self.clock.now() + Duration::seconds(expires_in.max(0));
        self.cache
            .lock()
            .expect("token cache lock")
            .insert(account, (token.to_owned(), profile.to_owned(), expires));
    }

    fn forget(&self, account: AccountId) {
        self.cache
            .lock()
            .expect("token cache lock")
            .remove(&account);
    }

    async fn mark_reconnect(&self, account: AccountId, message: &str) {
        let result = async {
            let mut profiles: HashMap<AccountId, ProfileState> = self
                .store
                .settings()
                .get(PROFILES_KEY)
                .await?
                .unwrap_or_default();
            if let Some(state) = profiles.get_mut(&account) {
                state.needs_reconnect = true;
                state.last_error = Some(message.to_owned());
                self.store.settings().set(PROFILES_KEY, &profiles).await?;
            }
            Ok::<_, shorts_store::StoreError>(())
        }
        .await;
        if let Err(e) = result {
            warn!(error = %e, "could not record that a Snapchat profile needs reconnecting");
        }
    }
}

#[async_trait]
impl SnapAuth for SnapTokenSource {
    async fn session(&self, account: AccountId) -> Result<SnapSession, AdapterError> {
        if let Some(session) = self.cached(account) {
            return Ok(session);
        }
        let _guard = self.refresh.lock().await;
        if let Some(session) = self.cached(account) {
            return Ok(session);
        }
        let store_err = |e: String| AdapterError::Transient {
            message: format!("credential store: {e}"),
            retry_after: None,
        };
        let Some(config) = self.app_config().map_err(store_err)? else {
            return Err(AdapterError::Authentication {
                message: "enter the Snap OAuth client id and secret in Settings".to_owned(),
            });
        };
        let Some(mut grant) = self.grant(account).map_err(store_err)? else {
            return Err(AdapterError::Authentication {
                message: "the profile is not signed in; connect it in Settings".to_owned(),
            });
        };
        match self
            .oauth
            .refresh(&config.client, &grant.refresh_token)
            .await
        {
            Ok(tokens) => {
                if let Some(rotated) = tokens.refresh_token.filter(|t| !t.is_empty()) {
                    grant.refresh_token = rotated;
                    if let Err(e) = self.save_grant(account, &grant) {
                        warn!(error = %e, "could not store a rotated Snap refresh token");
                    }
                }
                self.prime(
                    account,
                    &tokens.access_token,
                    &grant.profile_id,
                    tokens.expires_in,
                );
                Ok(SnapSession {
                    access_token: tokens.access_token,
                    profile_id: grant.profile_id,
                })
            }
            Err(SnapAuthError::Reauthorize { message } | SnapAuthError::BadClient { message }) => {
                let message = format!(
                    "Snap ended the profile's sign-in ({message}); reconnect it in Settings"
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
        if cache.get(&account).is_some_and(|(t, _, _)| t == token) {
            cache.remove(&account);
        }
    }
}

struct PendingConnect {
    id: u64,
    progress: SnapConnectProgress,
    cancel: CancellationToken,
    /// Paste mode: what's needed to finish once the owner pastes the code.
    paste: Option<PasteFlow>,
}

#[derive(Clone)]
struct PasteFlow {
    state: String,
    config: AppConfig,
    reconnect: Option<AccountId>,
}

/// The Snapchat connector.
pub struct SnapchatConnector {
    app: Arc<App>,
    oauth: SnapOAuth,
    adapter: Arc<SnapAdapter>,
    tokens: Arc<SnapTokenSource>,
    credentials: Arc<dyn CredentialStore>,
    pending: Mutex<Option<PendingConnect>>,
    next_id: Mutex<u64>,
}

impl SnapchatConnector {
    /// Builds the connector and registers its adapter with the app.
    pub fn new(
        app: Arc<App>,
        setup: SnapSetup,
        credentials: Arc<dyn CredentialStore>,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(setup.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        let oauth = SnapOAuth::new(http.clone(), setup.endpoints.clone());
        let clock = app.clock();
        let tokens = Arc::new(SnapTokenSource {
            oauth: oauth.clone(),
            credentials: credentials.clone(),
            store: app.store.clone(),
            clock: clock.clone(),
            cache: Mutex::new(HashMap::new()),
            refresh: tokio::sync::Mutex::new(()),
        });
        let adapter = Arc::new(SnapAdapter::new(
            http,
            SnapConfig {
                api_base: setup.api_base,
                locale: setup.locale,
                part_timeout: setup.part_timeout,
                ..SnapConfig::default()
            },
            tokens.clone(),
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

    async fn states(&self) -> AppResult<HashMap<AccountId, ProfileState>> {
        Ok(self
            .app
            .store
            .settings()
            .get(PROFILES_KEY)
            .await?
            .unwrap_or_default())
    }

    async fn save_states(&self, states: &HashMap<AccountId, ProfileState>) -> AppResult<()> {
        self.app.store.settings().set(PROFILES_KEY, states).await?;
        Ok(())
    }

    fn config(&self) -> AppResult<Option<AppConfig>> {
        self.tokens.app_config().map_err(credential_error)
    }

    pub async fn status(&self) -> AppResult<SnapStatus> {
        let config = self.config()?;
        let redirect_uri = config.as_ref().map_or_else(
            || DEFAULT_REDIRECT_URI.to_owned(),
            |c| c.redirect_uri.clone(),
        );
        let mode = Url::parse(&redirect_uri).map_or(ReturnMode::Paste, |u| return_mode(&u));
        let states = self.states().await?;
        let mut profiles = Vec::new();
        for account in self.app.store.accounts().list().await? {
            if account.platform != Platform::SnapchatSpotlight {
                continue;
            }
            let state = states.get(&account.id);
            let connected = account.credential_ref.is_some()
                && self
                    .tokens
                    .grant(account.id)
                    .map_err(credential_error)?
                    .is_some();
            profiles.push(SnapProfileView {
                account_id: account.id.to_string(),
                display_name: account.display_name.clone(),
                connected,
                needs_reconnect: connected && state.is_some_and(|s| s.needs_reconnect),
                last_error: state.and_then(|s| s.last_error.clone()),
                notes: account.capabilities.notes.clone(),
                connected_at: state.map(|s| s.connected_at),
            });
        }
        let (connecting, awaiting_code) = match self.pending.lock().expect("pending lock").as_ref()
        {
            Some(PendingConnect {
                progress: SnapConnectProgress::Pending,
                paste,
                ..
            }) => (paste.is_none(), paste.is_some()),
            _ => (false, false),
        };
        Ok(SnapStatus {
            client_configured: config.is_some(),
            redirect_uri,
            return_mode: mode,
            next_step: config.is_none().then(|| CLIENT_NEXT_STEP.to_owned()),
            connecting,
            awaiting_code,
            profiles,
        })
    }

    /// Saves the owner's Snap OAuth app in the credential store.
    pub async fn set_client(
        &self,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
    ) -> AppResult<SnapStatus> {
        let client = SnapClient::new(client_id, client_secret).map_err(AppError::Invalid)?;
        let redirect = parse_redirect(redirect_uri)?;
        let raw = serde_json::to_string(&AppConfig {
            client,
            redirect_uri: redirect.to_string(),
        })
        .expect("config serializes");
        self.credentials
            .set(CLIENT_KEY, &raw)
            .map_err(credential_error)?;
        info!("Snap OAuth app saved");
        self.app.emit(AppEvent::SnapchatChanged);
        self.status().await
    }

    pub async fn clear_client(&self) -> AppResult<SnapStatus> {
        self.credentials
            .delete(CLIENT_KEY)
            .map_err(credential_error)?;
        self.app.emit(AppEvent::SnapchatChanged);
        self.status().await
    }

    fn start_pending(&self, paste: Option<PasteFlow>) -> (u64, CancellationToken) {
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
                progress: SnapConnectProgress::Pending,
                cancel: cancel.clone(),
                paste,
            })
        {
            previous.cancel.cancel();
        }
        (id, cancel)
    }

    fn finish_pending(&self, id: u64, progress: SnapConnectProgress) {
        if let SnapConnectProgress::Failed { message } = &progress {
            warn!(%message, "Snap sign-in did not complete");
        }
        let mut pending = self.pending.lock().expect("pending lock");
        if let Some(p) = pending.as_mut().filter(|p| p.id == id) {
            p.progress = progress;
            p.paste = None;
        }
        drop(pending);
        self.app.emit(AppEvent::SnapchatChanged);
    }

    /// Starts the browser sign-in and returns Snap's consent page to open. With `account`,
    /// reconnects that profile.
    pub async fn begin_connect(self: &Arc<Self>, account: Option<AccountId>) -> AppResult<Url> {
        let config = self
            .config()?
            .ok_or_else(|| AppError::Invalid(CLIENT_NEXT_STEP.into()))?;
        if let Some(id) = account {
            let a = self.app.store.accounts().require(id).await?;
            if a.platform != Platform::SnapchatSpotlight {
                return Err(AppError::Invalid("that is not a Snapchat account".into()));
            }
        }
        let redirect = parse_redirect(&config.redirect_uri)?;
        let state = random_token();
        let url = self
            .oauth
            .authorization_url(&config.client, &config.redirect_uri, &state);
        match return_mode(&redirect) {
            ReturnMode::Paste => {
                self.start_pending(Some(PasteFlow {
                    state,
                    config,
                    reconnect: account,
                }));
            }
            ReturnMode::Loopback => {
                let port = redirect.port().expect("loopback addresses have a port");
                let loopback = Loopback::bind_port(port, redirect.path())
                    .await
                    .map_err(|e| {
                        AppError::Intake(format!(
                            "couldn't listen on port {port} for the Snap sign-in ({e}); close \
                         whatever uses it or register another port"
                        ))
                    })?;
                let (id, cancel) = self.start_pending(None);
                let this = self.clone();
                tokio::spawn(async move {
                    let outcome = tokio::select! {
                        () = cancel.cancelled() => return,
                        r = tokio::time::timeout(CONNECT_TIMEOUT, loopback.wait_for_code(&state)) => r,
                    };
                    let progress = match outcome {
                        Err(_) => SnapConnectProgress::Failed {
                            message: "The sign-in timed out. Start connecting again.".into(),
                        },
                        Ok(Err(e)) => SnapConnectProgress::Failed {
                            message: e.to_string().replace("Google", "Snap"),
                        },
                        Ok(Ok(code)) => match this.complete(&config, &code, account).await {
                            Ok(view) => SnapConnectProgress::Connected { account: view },
                            Err(e) => SnapConnectProgress::Failed {
                                message: e.to_string(),
                            },
                        },
                    };
                    this.finish_pending(id, progress);
                });
            }
        }
        self.app.emit(AppEvent::SnapchatChanged);
        Ok(url)
    }

    /// Finishes a paste-mode sign-in with the text the redirect page showed
    /// (`<state>.<code>`, or just the code).
    pub async fn finish_with_code(&self, pasted: &str) -> AppResult<AccountView> {
        let (id, flow) = {
            let pending = self.pending.lock().expect("pending lock");
            match pending.as_ref() {
                Some(PendingConnect {
                    id,
                    paste: Some(flow),
                    ..
                }) => (*id, flow.clone()),
                _ => {
                    return Err(AppError::Invalid(
                        "no Snap sign-in is waiting for a code; start connecting again".into(),
                    ));
                }
            }
        };
        let pasted = pasted.trim();
        let code = match pasted.split_once('.') {
            Some((state, code)) if state.len() >= 22 && !state.contains(['/', '+']) => {
                if state != flow.state {
                    return Err(AppError::Invalid(
                        "that code is from another sign-in; start connecting again".into(),
                    ));
                }
                code
            }
            _ => pasted,
        };
        if code.is_empty() || code.chars().any(char::is_whitespace) {
            return Err(AppError::Invalid(
                "paste the whole text shown on the page".into(),
            ));
        }
        let result = self.complete(&flow.config, code, flow.reconnect).await;
        self.finish_pending(
            id,
            match &result {
                Ok(view) => SnapConnectProgress::Connected {
                    account: view.clone(),
                },
                Err(e) => SnapConnectProgress::Failed {
                    message: e.to_string(),
                },
            },
        );
        // The outcome is returned directly; don't also report it through `connect_progress`.
        self.pending.lock().expect("pending lock").take();
        result
    }

    /// Where a loopback sign-in stands; call it until it isn't `pending`.
    pub fn connect_progress(&self) -> SnapConnectProgress {
        let mut pending = self.pending.lock().expect("pending lock");
        match pending.as_ref().map(|p| p.progress.clone()) {
            None => SnapConnectProgress::Idle,
            Some(SnapConnectProgress::Pending) => SnapConnectProgress::Pending,
            Some(done) => {
                pending.take();
                done
            }
        }
    }

    pub fn cancel_connect(&self) {
        if let Some(p) = self.pending.lock().expect("pending lock").take() {
            p.cancel.cancel();
        }
        self.app.emit(AppEvent::SnapchatChanged);
    }

    async fn complete(
        &self,
        config: &AppConfig,
        code: &str,
        reconnect: Option<AccountId>,
    ) -> AppResult<AccountView> {
        let tokens = self
            .oauth
            .exchange_code(&config.client, code, &config.redirect_uri)
            .await
            .map_err(|e| AppError::Intake(e.to_string()))?;
        let Some(refresh_token) = tokens.refresh_token.clone().filter(|t| !t.is_empty()) else {
            return Err(AppError::Invalid(
                "Snap didn't return a long-lived sign-in; connect again".into(),
            ));
        };
        let profile = self
            .adapter
            .my_profile(&tokens.access_token)
            .await
            .map_err(AppError::Adapter)?;

        let accounts = self.app.store.accounts().list().await?;
        let mut existing = None;
        for a in accounts
            .iter()
            .filter(|a| a.platform == Platform::SnapchatSpotlight)
        {
            let grant = self.tokens.grant(a.id).map_err(credential_error)?;
            if grant.is_some_and(|g| g.profile_id == profile.id) {
                existing = Some(a.clone());
                break;
            }
        }
        let name = if profile.display_name.trim().is_empty() {
            format!("@{}", profile.username)
        } else {
            profile.display_name.clone()
        };
        let target = match (reconnect, existing) {
            (Some(id), Some(found)) if found.id != id => {
                return Err(AppError::Invalid(format!(
                    "You signed in to {name}, which is already connected as \"{}\". Reconnect \
                     that profile instead.",
                    found.display_name
                )));
            }
            (Some(id), _) => {
                let account = self.app.store.accounts().require(id).await?;
                let previous = self.tokens.grant(id).map_err(credential_error)?;
                if previous.is_some_and(|g| g.profile_id != profile.id) {
                    return Err(AppError::Invalid(format!(
                        "You signed in to {name}, a different profile from \"{}\".",
                        account.display_name
                    )));
                }
                Some(account)
            }
            (None, found) => found,
        };
        let now = self.app.now();
        let mut account = match target {
            Some(a) => a,
            None => ConnectedAccount::manual(Platform::SnapchatSpotlight, &name, now),
        };
        let is_new = self.app.store.accounts().get(account.id).await?.is_none();
        let grant = Grant {
            refresh_token,
            profile_id: profile.id.clone(),
            granted_at: now,
        };
        self.tokens
            .save_grant(account.id, &grant)
            .map_err(credential_error)?;
        self.tokens.prime(
            account.id,
            &tokens.access_token,
            &profile.id,
            tokens.expires_in,
        );
        account.credential_ref = Some(grant_key(account.id));
        account.capabilities = self.adapter.capabilities(&account).await?;
        account.updated_at = now;
        if is_new {
            self.app.store.accounts().insert(&account).await?;
        } else {
            self.app.store.accounts().update(&account).await?;
        }
        let mut states = self.states().await?;
        states.insert(
            account.id,
            ProfileState {
                connected_at: now,
                needs_reconnect: false,
                last_error: None,
            },
        );
        self.save_states(&states).await?;
        info!(account = %account.id, "Snapchat profile connected");
        self.resume_waiting(account.id).await?;
        Ok(AccountView::from(&account))
    }

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

    /// Re-checks the profile's sign-in (refreshing its token) and updates its capability
    /// record, for the "Check connection" button.
    pub async fn check_connection(&self, account_id: AccountId) -> AppResult<SnapStatus> {
        let mut account = self.app.store.accounts().require(account_id).await?;
        if account.platform != Platform::SnapchatSpotlight {
            return Err(AppError::Invalid("that is not a Snapchat account".into()));
        }
        if account.credential_ref.is_some() {
            account.capabilities = self.adapter.capabilities(&account).await?;
            account.updated_at = self.app.now();
            self.app.store.accounts().update(&account).await?;
        }
        self.app.emit(AppEvent::SnapchatChanged);
        self.status().await
    }

    /// Forgets the profile's sign-in. The destination stays, using export packages.
    pub async fn disconnect(&self, account_id: AccountId) -> AppResult<SnapStatus> {
        let mut account = self.app.store.accounts().require(account_id).await?;
        if account.platform != Platform::SnapchatSpotlight {
            return Err(AppError::Invalid("that is not a Snapchat account".into()));
        }
        self.credentials
            .delete(&grant_key(account_id))
            .map_err(credential_error)?;
        self.tokens.forget(account_id);
        account.capabilities = Capabilities::reconnect_required(
            "Disconnected. Connect the profile again to post automatically; until then its \
             publications use export packages.",
            self.app.now(),
        );
        account.credential_ref = None;
        account.updated_at = self.app.now();
        self.app.store.accounts().update(&account).await?;
        let mut states = self.states().await?;
        states.remove(&account_id);
        self.save_states(&states).await?;
        self.app.emit(AppEvent::SnapchatChanged);
        self.status().await
    }
}
