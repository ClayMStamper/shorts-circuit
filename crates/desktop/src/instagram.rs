//! Instagram account connections: a long-lived token the owner generates in their Meta app's
//! dashboard, renewed automatically before its 60 days run out, and handed to the Reels adapter.
//!
//! Connecting by pasted token needs no redirect address, no hosting and no app secret: Meta's
//! dashboard issues tokens for the Instagram accounts added to the app (as testers while it is
//! in development), and a long-lived token at least a day old can be renewed with itself.
//!
//! Secrets live in the OS credential store only (`instagram:<account id>`: the token and the
//! account's Instagram user id). SQLite holds the account, its capability record and a small
//! status record per account; views never include tokens or the user id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use shorts_adapters::instagram::{
    DEFAULT_API_VERSION, GRAPH_BASE, IgSession, InstagramAdapter, InstagramAuth, InstagramConfig,
    UPLOAD_BASE,
};
use shorts_adapters::{Adapter, AdapterError};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, ExecutionOwner, Platform, PublicationEvent,
    PublicationState, instagram,
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

const ACCOUNTS_KEY: &str = "instagram_accounts";
/// Renew a token when it has less than this left (and is old enough to be renewed).
const RENEW_WITHIN: Duration = Duration::days(20);
/// How often the background task looks for tokens to renew.
const RENEW_EVERY: StdDuration = StdDuration::from_secs(6 * 60 * 60);

pub const CONNECT_NEXT_STEP: &str = "Create a Meta app with the Instagram API (Instagram \
    login), add your Instagram professional account to it, generate an access token for it in \
    the app dashboard, and paste the token here (see docs/human-scope.md).";

fn grant_key(account: AccountId) -> String {
    format!("instagram:{account}")
}

fn credential_error(e: impl std::fmt::Display) -> AppError {
    AppError::Intake(format!("credential store: {e}"))
}

/// Where Instagram's endpoints are.
#[derive(Debug, Clone)]
pub struct InstagramSetup {
    pub graph_base: Url,
    pub upload_base: Url,
    pub api_version: String,
    pub request_timeout: StdDuration,
    pub upload_timeout: StdDuration,
    pub poll_every: StdDuration,
}

impl Default for InstagramSetup {
    fn default() -> Self {
        let config = InstagramConfig::default();
        Self {
            graph_base: Url::parse(GRAPH_BASE).expect("valid URL"),
            upload_base: Url::parse(UPLOAD_BASE).expect("valid URL"),
            api_version: DEFAULT_API_VERSION.to_owned(),
            request_timeout: StdDuration::from_secs(60),
            upload_timeout: config.upload_timeout,
            poll_every: config.poll_every,
        }
    }
}

impl InstagramSetup {
    /// The defaults, with the Graph API version taken from `SHORTS_INSTAGRAM_API_VERSION`
    /// when set (e.g. `v25.0`).
    pub fn from_env() -> Self {
        let mut setup = Self::default();
        if let Ok(v) = std::env::var("SHORTS_INSTAGRAM_API_VERSION") {
            let v = v.trim();
            if v.starts_with('v') && v.len() <= 8 {
                setup.api_version = v.to_owned();
            }
        }
        setup
    }
}

/// An account's token, kept in the credential store.
#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    access_token: String,
    user_id: String,
    username: String,
    /// When this token was issued or last renewed.
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// Non-secret per-account status, kept in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AccountState {
    connected_at: DateTime<Utc>,
    token_expires_at: DateTime<Utc>,
    renewed_at: Option<DateTime<Utc>>,
    needs_reconnect: bool,
    last_error: Option<String>,
}

/// One Instagram destination as the Settings screen shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstagramAccount {
    pub account_id: String,
    pub display_name: String,
    pub connected: bool,
    pub needs_reconnect: bool,
    pub last_error: Option<String>,
    pub notes: Vec<String>,
    pub connected_at: Option<DateTime<Utc>>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub renewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstagramStatus {
    pub accounts: Vec<InstagramAccount>,
    /// What to do first, while no account is connected.
    pub next_step: Option<String>,
}

/// Tokens from the credential store, renewed when they near expiry.
pub(crate) struct InstagramTokens {
    adapter: Mutex<Option<Arc<InstagramAdapter>>>,
    credentials: Arc<dyn CredentialStore>,
    store: Store,
    clock: Arc<dyn shorts_scheduler::Clock>,
    renew: tokio::sync::Mutex<()>,
}

impl InstagramTokens {
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

    fn adapter(&self) -> Option<Arc<InstagramAdapter>> {
        self.adapter.lock().expect("adapter lock").clone()
    }

    async fn update_state(&self, account: AccountId, f: impl FnOnce(&mut AccountState)) {
        let result = async {
            let mut accounts: HashMap<AccountId, AccountState> = self
                .store
                .settings()
                .get(ACCOUNTS_KEY)
                .await?
                .unwrap_or_default();
            if let Some(state) = accounts.get_mut(&account) {
                f(state);
                self.store.settings().set(ACCOUNTS_KEY, &accounts).await?;
            }
            Ok::<_, shorts_store::StoreError>(())
        }
        .await;
        if let Err(e) = result {
            warn!(error = %e, "could not record an Instagram account's status");
        }
    }

    async fn mark_reconnect(&self, account: AccountId, message: &str) {
        self.update_state(account, |s| {
            s.needs_reconnect = true;
            s.last_error = Some(message.to_owned());
        })
        .await;
    }

    /// Renews the token if it is due: less than [`RENEW_WITHIN`] left and old enough for
    /// Instagram to renew. A failed renewal leaves the current token in use while it lasts.
    async fn renew_if_due(&self, account: AccountId, grant: Grant) -> Result<Grant, AdapterError> {
        let now = self.clock.now();
        let due = grant.expires_at - now < RENEW_WITHIN
            && now - grant.issued_at >= Duration::hours(instagram::TOKEN_MIN_AGE_HOURS);
        if !due {
            return Ok(grant);
        }
        let Some(adapter) = self.adapter() else {
            return Ok(grant);
        };
        let _guard = self.renew.lock().await;
        // Another caller may have renewed it meanwhile.
        let grant = match self.grant(account) {
            Ok(Some(g)) if g.access_token != grant.access_token => return Ok(g),
            _ => grant,
        };
        match adapter.refresh_token(&grant.access_token).await {
            Ok(renewed) => {
                let lifetime = if renewed.expires_in > 0 {
                    Duration::seconds(renewed.expires_in)
                } else {
                    Duration::days(instagram::TOKEN_LIFETIME_DAYS)
                };
                let grant = Grant {
                    access_token: renewed.access_token,
                    issued_at: now,
                    expires_at: now + lifetime,
                    ..grant
                };
                if let Err(e) = self.save_grant(account, &grant) {
                    warn!(error = %e, "could not store a renewed Instagram token");
                    return Ok(grant);
                }
                let expires = grant.expires_at;
                self.update_state(account, |s| {
                    s.token_expires_at = expires;
                    s.renewed_at = Some(now);
                    s.needs_reconnect = false;
                    s.last_error = None;
                })
                .await;
                info!(account = %account, "Instagram token renewed");
                Ok(grant)
            }
            Err(AdapterError::Authentication { message }) => {
                let message = format!(
                    "Instagram would not renew the sign-in ({message}); generate a new token in \
                     the Meta app dashboard and reconnect"
                );
                self.mark_reconnect(account, &message).await;
                Err(AdapterError::Authentication { message })
            }
            Err(e) => {
                warn!(error = %e, "could not renew an Instagram token; using the current one");
                Ok(grant)
            }
        }
    }
}

#[async_trait]
impl InstagramAuth for InstagramTokens {
    async fn session(&self, account: AccountId) -> Result<IgSession, AdapterError> {
        let grant = self
            .grant(account)
            .map_err(|e| AdapterError::Transient {
                message: format!("credential store: {e}"),
                retry_after: None,
            })?
            .ok_or_else(|| AdapterError::Authentication {
                message: "the account is not connected; connect it in Settings".to_owned(),
            })?;
        if grant.expires_at <= self.clock.now() {
            let message = "the Instagram sign-in ran out (tokens last 60 days unless renewed); \
                           generate a new token in the Meta app dashboard and reconnect"
                .to_owned();
            self.mark_reconnect(account, &message).await;
            return Err(AdapterError::Authentication { message });
        }
        let grant = self.renew_if_due(account, grant).await?;
        Ok(IgSession {
            access_token: grant.access_token,
            user_id: grant.user_id,
        })
    }

    async fn invalidate(&self, account: AccountId, _token: &str) {
        // A long-lived token can't be swapped for a new one without the owner; the adapter
        // retries once and then reports the account as needing a reconnect.
        self.mark_reconnect(
            account,
            "Instagram refused the saved token; generate a new one in the Meta app dashboard \
             and reconnect",
        )
        .await;
    }
}

/// The Instagram connector.
pub struct InstagramConnector {
    app: Arc<App>,
    adapter: Arc<InstagramAdapter>,
    tokens: Arc<InstagramTokens>,
    credentials: Arc<dyn CredentialStore>,
}

impl InstagramConnector {
    /// Builds the connector and registers its adapter with the app.
    pub fn new(
        app: Arc<App>,
        setup: InstagramSetup,
        credentials: Arc<dyn CredentialStore>,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(setup.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        let clock = app.clock();
        let tokens = Arc::new(InstagramTokens {
            adapter: Mutex::new(None),
            credentials: credentials.clone(),
            store: app.store.clone(),
            clock: clock.clone(),
            renew: tokio::sync::Mutex::new(()),
        });
        let adapter = Arc::new(InstagramAdapter::new(
            http,
            InstagramConfig {
                graph_base: setup.graph_base,
                upload_base: setup.upload_base,
                api_version: setup.api_version,
                poll_every: setup.poll_every,
                upload_timeout: setup.upload_timeout,
                ..InstagramConfig::default()
            },
            tokens.clone(),
            Arc::new(move || clock.now()),
        ));
        *tokens.adapter.lock().expect("adapter lock") = Some(adapter.clone());
        app.register_adapter(adapter.clone());
        Arc::new(Self {
            app,
            adapter,
            tokens,
            credentials,
        })
    }

    async fn states(&self) -> AppResult<HashMap<AccountId, AccountState>> {
        Ok(self
            .app
            .store
            .settings()
            .get(ACCOUNTS_KEY)
            .await?
            .unwrap_or_default())
    }

    async fn save_states(&self, states: &HashMap<AccountId, AccountState>) -> AppResult<()> {
        self.app.store.settings().set(ACCOUNTS_KEY, states).await?;
        Ok(())
    }

    pub async fn status(&self) -> AppResult<InstagramStatus> {
        let states = self.states().await?;
        let mut accounts = Vec::new();
        for account in self.app.store.accounts().list().await? {
            if account.platform != Platform::InstagramReels {
                continue;
            }
            let state = states.get(&account.id);
            let connected = account.credential_ref.is_some()
                && self
                    .tokens
                    .grant(account.id)
                    .map_err(credential_error)?
                    .is_some();
            accounts.push(InstagramAccount {
                account_id: account.id.to_string(),
                display_name: account.display_name.clone(),
                connected,
                needs_reconnect: connected && state.is_some_and(|s| s.needs_reconnect),
                last_error: state.and_then(|s| s.last_error.clone()),
                notes: account.capabilities.notes.clone(),
                connected_at: state.map(|s| s.connected_at),
                token_expires_at: state.map(|s| s.token_expires_at),
                renewed_at: state.and_then(|s| s.renewed_at),
            });
        }
        let next_step =
            (!accounts.iter().any(|a| a.connected)).then(|| CONNECT_NEXT_STEP.to_owned());
        Ok(InstagramStatus {
            accounts,
            next_step,
        })
    }

    /// Connects (or, with `account`, reconnects) the professional account a pasted long-lived
    /// token belongs to.
    pub async fn connect(
        &self,
        access_token: &str,
        reconnect: Option<AccountId>,
    ) -> AppResult<AccountView> {
        let token = access_token.trim();
        if token.len() < 20 || token.chars().any(char::is_whitespace) {
            return Err(AppError::Invalid(
                "paste the whole access token from the Meta app dashboard".into(),
            ));
        }
        let profile = self.adapter.profile(token).await.map_err(|e| match e {
            AdapterError::Authentication { .. } => AppError::Invalid(
                "Instagram didn't accept that token. Generate a new one in the Meta app \
                 dashboard (Instagram > API setup with Instagram login) and paste it again."
                    .into(),
            ),
            other => AppError::Adapter(other),
        })?;
        if !profile.can_publish() {
            return Err(AppError::Invalid(format!(
                "@{} is a personal account. Switch it to a professional (creator or business) \
                 account in the Instagram app, then connect again.",
                profile.username
            )));
        }
        let user_id = profile.publishing_id().to_owned();
        if user_id.is_empty() {
            return Err(AppError::Intake(
                "Instagram didn't say which account the token belongs to".into(),
            ));
        }

        // Reuse the destination this account already has, if any.
        let accounts = self.app.store.accounts().list().await?;
        let mut existing = None;
        for a in accounts
            .iter()
            .filter(|a| a.platform == Platform::InstagramReels)
        {
            let grant = self.tokens.grant(a.id).map_err(credential_error)?;
            if grant.is_some_and(|g| g.user_id == user_id) {
                existing = Some(a.clone());
                break;
            }
        }
        let display = format!("@{}", profile.username);
        let target = match (reconnect, existing) {
            (Some(id), Some(found)) if found.id != id => {
                return Err(AppError::Invalid(format!(
                    "That token is for {display}, which is already connected as \"{}\". \
                     Reconnect that account instead.",
                    found.display_name
                )));
            }
            (Some(id), _) => {
                let account = self.app.store.accounts().require(id).await?;
                if account.platform != Platform::InstagramReels {
                    return Err(AppError::Invalid("that is not an Instagram account".into()));
                }
                let previous = self.tokens.grant(id).map_err(credential_error)?;
                if previous.is_some_and(|g| g.user_id != user_id) {
                    return Err(AppError::Invalid(format!(
                        "That token is for {display}, a different account from \"{}\". Use a \
                         token for that account, or connect the new one separately.",
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
            None => ConnectedAccount::manual(Platform::InstagramReels, &display, now),
        };
        let is_new = self.app.store.accounts().get(account.id).await?.is_none();
        let grant = Grant {
            access_token: token.to_owned(),
            user_id,
            username: profile.username.clone(),
            issued_at: now,
            expires_at: now + Duration::days(instagram::TOKEN_LIFETIME_DAYS),
        };
        self.tokens
            .save_grant(account.id, &grant)
            .map_err(credential_error)?;
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
            AccountState {
                connected_at: now,
                token_expires_at: grant.expires_at,
                renewed_at: None,
                needs_reconnect: false,
                last_error: None,
            },
        );
        self.save_states(&states).await?;
        info!(account = %account.id, "Instagram account connected");
        self.resume_waiting(account.id).await?;
        self.app.emit(AppEvent::InstagramChanged);
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

    /// Forgets the account's token. The destination stays, using export packages until it is
    /// connected again. (To revoke the app's access entirely, remove it in Instagram's
    /// settings under Website permissions / Apps.)
    pub async fn disconnect(&self, account_id: AccountId) -> AppResult<InstagramStatus> {
        let mut account = self.app.store.accounts().require(account_id).await?;
        if account.platform != Platform::InstagramReels {
            return Err(AppError::Invalid("that is not an Instagram account".into()));
        }
        self.credentials
            .delete(&grant_key(account_id))
            .map_err(credential_error)?;
        account.capabilities = Capabilities::reconnect_required(
            "Disconnected. Connect the account again to publish automatically; until then its \
             publications use export packages.",
            self.app.now(),
        );
        account.credential_ref = None;
        account.updated_at = self.app.now();
        self.app.store.accounts().update(&account).await?;
        let mut states = self.states().await?;
        states.remove(&account_id);
        self.save_states(&states).await?;
        self.app.emit(AppEvent::InstagramChanged);
        self.status().await
    }

    /// Renews every connected account's token that is due. Returns how many were renewed.
    pub async fn renew_due(&self) -> AppResult<usize> {
        let mut renewed = 0;
        for account in self.app.store.accounts().list().await? {
            if account.platform != Platform::InstagramReels || account.credential_ref.is_none() {
                continue;
            }
            let Some(grant) = self.tokens.grant(account.id).map_err(credential_error)? else {
                continue;
            };
            let before = grant.access_token.clone();
            if grant.expires_at <= self.app.now() {
                // `session` records that it ran out.
                let _ = self.tokens.session(account.id).await;
                continue;
            }
            match self.tokens.renew_if_due(account.id, grant).await {
                Ok(g) if g.access_token != before => renewed += 1,
                Ok(_) => {}
                Err(e) => warn!(account = %account.id, error = %e, "Instagram token not renewed"),
            }
        }
        if renewed > 0 {
            self.app.emit(AppEvent::InstagramChanged);
        }
        Ok(renewed)
    }

    /// Keeps tokens renewed while the app runs, so an account that isn't posting often doesn't
    /// lose its sign-in.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            if let Err(e) = self.renew_due().await {
                warn!(error = %e, "could not check Instagram tokens");
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(RENEW_EVERY) => {}
            }
        }
    }
}

// --- cloud runner ----------------------------------------------------------------------------

/// The account's long-lived token for the cloud runner.
pub(crate) fn cloud_credential(
    credentials: &dyn CredentialStore,
    account: AccountId,
) -> Result<Option<CloudCredential>, String> {
    let grant: Option<Grant> = credentials
        .get(&grant_key(account))
        .map_err(|e| e.to_string())?
        .and_then(|r| serde_json::from_str(&r).ok());
    Ok(grant.map(|g| CloudCredential::InstagramReels {
        access_token: Secret::new(g.access_token),
        user_id: g.user_id,
        issued_at: g.issued_at,
        expires_at: g.expires_at,
    }))
}

/// Keeps a token the cloud runner renewed.
pub(crate) fn import_cloud_credential(
    credentials: &dyn CredentialStore,
    account: AccountId,
    credential: &CloudCredential,
) -> Result<(), String> {
    let CloudCredential::InstagramReels {
        access_token,
        user_id,
        issued_at,
        expires_at,
    } = credential
    else {
        return Err("not an Instagram credential".into());
    };
    let key = grant_key(account);
    let Some(mut grant) = credentials
        .get(&key)
        .map_err(|e| e.to_string())?
        .and_then(|r| serde_json::from_str::<Grant>(&r).ok())
    else {
        return Ok(());
    };
    if &grant.user_id != user_id {
        return Err("the cloud runner's credential is for another account".into());
    }
    if *expires_at <= grant.expires_at {
        return Ok(());
    }
    grant.access_token = access_token.expose().to_owned();
    grant.issued_at = *issued_at;
    grant.expires_at = *expires_at;
    let raw = serde_json::to_string(&grant).expect("grant serializes");
    credentials.set(&key, &raw).map_err(|e| e.to_string())
}
