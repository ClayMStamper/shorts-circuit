//! Instagram account connections through the linked Facebook Page (Instagram API with Facebook
//! Login), handed to the Reels adapter.
//!
//! The owner generates a user token with their own Meta app in Graph API Explorer, extends it
//! to a long-lived one in the Access Token Debugger and pastes it. The app checks its
//! permissions (`/debug_token`, else `/me/permissions`), lists the Pages it manages that have a
//! linked Instagram professional account (`/me/accounts`), and keeps the chosen Page's token,
//! which doesn't expire when it comes from a long-lived user token. No redirect address,
//! hosting or app secret is needed, and nothing has to be renewed; a token Meta ends anyway
//! (code 190) asks for a reconnect.
//!
//! Accounts connected with Instagram Login (a pasted `IGAA…` token) stay as a fallback
//! connection type: Instagram only takes a public `video_url` that way, so they use export
//! packages and are marked "reconnect with Facebook login". Accounts connected that way before
//! Facebook login existed are migrated to that state on start.
//!
//! Secrets live in the OS credential store only (`instagram:<account id>`: the Page token and
//! the Instagram account id). SQLite holds the account, its capability record and a small
//! status record per account; views never include tokens or ids.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use shorts_adapters::instagram::{
    DEFAULT_API_VERSION, FbPage, GRAPH_BASE, INSTAGRAM_LOGIN_BASE, IgSession, InstagramAdapter,
    InstagramAuth, InstagramConfig, UPLOAD_BASE,
};
use shorts_adapters::{Adapter, AdapterError};
use shorts_domain::instagram::{
    BUSINESS_PERMISSION, InstagramLogin, LONG_LIVED_MIN_HOURS, REQUIRED_PERMISSIONS,
};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, ExecutionOwner, Platform, PublicationEvent,
    PublicationState,
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

pub const CONNECT_NEXT_STEP: &str = "Link your Instagram professional account to a Facebook \
    Page, generate a token for your Meta app in Graph API Explorer, extend it in the Access \
    Token Debugger, and paste it here (see docs/human-scope.md).";

/// Why an Instagram Login account can't publish automatically.
pub const INSTAGRAM_LOGIN_LIMIT: &str = "Connected with Instagram login, which can't upload \
    files directly (Instagram asks for a public video URL). Reconnect with Facebook login to \
    publish automatically; until then its publications use export packages.";

const TOKEN_HELP: &str = "Generate a new one in Graph API Explorer with your Meta app, extend \
    it in the Access Token Debugger, and paste it again.";

fn grant_key(account: AccountId) -> String {
    format!("instagram:{account}")
}

fn credential_error(e: impl std::fmt::Display) -> AppError {
    AppError::Intake(format!("credential store: {e}"))
}

/// Where Meta's endpoints are.
#[derive(Debug, Clone)]
pub struct InstagramSetup {
    /// `graph.facebook.com`.
    pub graph_base: Url,
    /// `graph.instagram.com`, for the Instagram Login fallback.
    pub instagram_login_base: Url,
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
            instagram_login_base: Url::parse(INSTAGRAM_LOGIN_BASE).expect("valid URL"),
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

/// An account's token, kept in the credential store. Grants written before Facebook login
/// existed have no `login` and read as Instagram Login (their old expiry fields are ignored).
#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    #[serde(default)]
    login: InstagramLogin,
    /// The Page token (Facebook login) or the Instagram Login token.
    access_token: String,
    /// The Instagram account id.
    user_id: String,
    username: String,
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    page_name: Option<String>,
}

/// Non-secret per-account status, kept in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AccountState {
    connected_at: DateTime<Utc>,
    needs_reconnect: bool,
    last_error: Option<String>,
}

/// One Instagram destination as Connections shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstagramAccount {
    pub account_id: String,
    pub display_name: String,
    pub connected: bool,
    pub needs_reconnect: bool,
    /// How it is connected, while it is.
    pub login: Option<InstagramLogin>,
    /// Connected with Facebook login and able to publish.
    pub can_publish: bool,
    pub page_name: Option<String>,
    pub username: Option<String>,
    pub last_error: Option<String>,
    pub notes: Vec<String>,
    pub connected_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstagramStatus {
    pub accounts: Vec<InstagramAccount>,
    /// What to do first, while no account publishes.
    pub next_step: Option<String>,
}

/// A Page a pasted token manages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstagramPageChoice {
    pub page_id: String,
    pub page_name: String,
    /// The linked Instagram professional account, if any.
    pub instagram_username: Option<String>,
}

/// The Pages a pasted token can connect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstagramPages {
    /// Pages with a linked Instagram account first.
    pub pages: Vec<InstagramPageChoice>,
    /// Said when the token's lifetime couldn't be read.
    pub warning: Option<String>,
}

/// Page tokens from the credential store.
pub(crate) struct InstagramTokens {
    credentials: Arc<dyn CredentialStore>,
    store: Store,
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
                message: "the account is not connected; connect it in Connections".to_owned(),
            })?;
        if grant.login != InstagramLogin::FacebookLogin {
            return Err(AdapterError::Authentication {
                message: INSTAGRAM_LOGIN_LIMIT.to_owned(),
            });
        }
        Ok(IgSession {
            access_token: grant.access_token,
            user_id: grant.user_id,
        })
    }

    async fn invalidate(&self, account: AccountId, _token: &str) {
        // A Page token can't be replaced without the owner; the adapter retries once and
        // then reports the account as needing a reconnect.
        self.update_state(account, |s| {
            s.needs_reconnect = true;
            s.last_error = Some(
                "Meta refused the saved Page token; paste a new token with Reconnect".to_owned(),
            );
        })
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

fn token_shape(token: &str) -> AppResult<&str> {
    let token = token.trim();
    if token.len() < 20 || token.chars().any(char::is_whitespace) {
        return Err(AppError::Invalid(
            "paste the whole access token (it is long; copy it with the copy button)".into(),
        ));
    }
    Ok(token)
}

fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
            credentials: credentials.clone(),
            store: app.store.clone(),
        });
        let adapter = Arc::new(InstagramAdapter::new(
            http,
            InstagramConfig {
                graph_base: setup.graph_base,
                instagram_login_base: setup.instagram_login_base,
                upload_base: setup.upload_base,
                api_version: setup.api_version,
                poll_every: setup.poll_every,
                upload_timeout: setup.upload_timeout,
                ..InstagramConfig::default()
            },
            tokens.clone(),
            Arc::new(move || clock.now()),
        ));
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

    /// Accounts connected with Instagram Login whose capability record still says they publish
    /// automatically (they were connected before Facebook login existed) switch to export
    /// packages and ask for a reconnect with Facebook login. The account, its token, its
    /// workflows and their Auto Publish grants stay. Returns how many changed.
    pub async fn migrate(&self) -> AppResult<usize> {
        let mut changed = 0;
        for mut account in self.app.store.accounts().list().await? {
            if account.platform != Platform::InstagramReels
                || !account.capabilities.can_publish_automatically()
            {
                continue;
            }
            let grant = self.tokens.grant(account.id).map_err(credential_error)?;
            if grant.is_some_and(|g| g.login == InstagramLogin::FacebookLogin) {
                continue;
            }
            account.capabilities = self.adapter.capabilities(&account).await?;
            account.updated_at = self.app.now();
            self.app.store.accounts().update(&account).await?;
            info!(account = %account.id, "Instagram account now waits for Facebook login");
            changed += 1;
        }
        if changed > 0 {
            self.app.emit(AppEvent::InstagramChanged);
        }
        Ok(changed)
    }

    pub async fn status(&self) -> AppResult<InstagramStatus> {
        self.migrate().await?;
        let states = self.states().await?;
        let mut accounts = Vec::new();
        for account in self.app.store.accounts().list().await? {
            if account.platform != Platform::InstagramReels {
                continue;
            }
            let state = states.get(&account.id);
            let grant = match account.credential_ref {
                Some(_) => self.tokens.grant(account.id).map_err(credential_error)?,
                None => None,
            };
            let connected = grant.is_some();
            let login = grant.as_ref().map(|g| g.login);
            let facebook = login == Some(InstagramLogin::FacebookLogin);
            let needs_reconnect = connected
                && (!facebook
                    || state.is_some_and(|s| s.needs_reconnect)
                    || !account.capabilities.can_publish_automatically());
            let last_error = if connected && !facebook {
                Some(INSTAGRAM_LOGIN_LIMIT.to_owned())
            } else {
                state.and_then(|s| s.last_error.clone())
            };
            accounts.push(InstagramAccount {
                account_id: account.id.to_string(),
                display_name: account.display_name.clone(),
                connected,
                needs_reconnect,
                login,
                can_publish: facebook && account.capabilities.can_publish_automatically(),
                page_name: grant.as_ref().and_then(|g| g.page_name.clone()),
                username: grant.as_ref().map(|g| g.username.clone()),
                last_error,
                notes: account.capabilities.notes.clone(),
                connected_at: state.map(|s| s.connected_at),
            });
        }
        let next_step =
            (!accounts.iter().any(|a| a.can_publish)).then(|| CONNECT_NEXT_STEP.to_owned());
        Ok(InstagramStatus {
            accounts,
            next_step,
        })
    }

    /// Checks a pasted user token and reads the Pages it manages. Every refusal names the one
    /// thing to do next.
    async fn usable_pages(&self, user_token: &str) -> AppResult<(Vec<FbPage>, Option<String>)> {
        if user_token.starts_with("IGAA") {
            return Err(AppError::Invalid(
                "That is an Instagram login token (it starts with IGAA). For Facebook login, \
                 generate a token in Graph API Explorer with your Meta app; or choose \
                 \"Instagram login\" (export packages only)."
                    .into(),
            ));
        }
        let refused = |message: String| {
            AppError::Invalid(format!(
                "Meta didn't accept that token ({message}). {TOKEN_HELP}"
            ))
        };
        let check = self
            .adapter
            .check_token(user_token)
            .await
            .map_err(|e| match e {
                AdapterError::Authentication { message } => refused(message),
                other => AppError::Adapter(other),
            })?;
        let missing: Vec<&str> = REQUIRED_PERMISSIONS
            .iter()
            .copied()
            .filter(|p| !check.scopes.iter().any(|s| s == p))
            .collect();
        if !missing.is_empty() {
            return Err(AppError::Invalid(format!(
                "The token is missing the {} permission{}. In Graph API Explorer, add {} under \
                 Permissions, click Generate Access Token and allow them, then extend it and \
                 paste it again.",
                join_names(&missing),
                if missing.len() == 1 { "" } else { "s" },
                if missing.len() == 1 { "it" } else { "them" },
            )));
        }
        let now = self.app.now();
        if let Some(expires) = check.expires_at
            && expires - now < Duration::hours(LONG_LIVED_MIN_HOURS)
        {
            return Err(AppError::Invalid(format!(
                "That token is short-lived (it expires in {} minutes), so the Page token would \
                 expire too. Open it in the Access Token Debugger, click Extend Access Token at \
                 the bottom, and paste the new token.",
                (expires - now).num_minutes().max(0)
            )));
        }
        let warning = (!check.expiry_known).then(|| {
            "Meta didn't say how long this token lasts. In the Access Token Debugger, \
             \"Expires\" should say Never or about 60 days; if it says about an hour, click \
             Extend Access Token and reconnect with the new token."
                .to_owned()
        });
        let pages = self.adapter.pages(user_token).await.map_err(|e| match e {
            AdapterError::Authentication { message } => refused(message),
            other => AppError::Adapter(other),
        })?;
        if pages.is_empty() {
            let business = if check.scopes.iter().any(|s| s == BUSINESS_PERMISSION) {
                String::new()
            } else {
                format!(" If the Page is in a business portfolio, add {BUSINESS_PERMISSION} too.")
            };
            return Err(AppError::Invalid(format!(
                "The token can't see any Facebook Pages. Generate it again in Graph API Explorer \
                 and, when Facebook asks which Pages to allow, choose the Page your Instagram \
                 account is linked to (you need to be its admin).{business}"
            )));
        }
        if !pages.iter().any(|p| p.instagram_business_account.is_some()) {
            let names: Vec<&str> = pages.iter().map(|p| p.name.as_str()).collect();
            return Err(AppError::Invalid(format!(
                "{} {} no linked Instagram account. Link it in the Page's settings → Linked \
                 accounts → Instagram (the Instagram account must be a professional one), then \
                 paste the token again.",
                join_names(&names),
                if names.len() == 1 { "has" } else { "have" },
            )));
        }
        Ok((pages, warning))
    }

    /// The Pages a pasted user token can connect, for choosing one. Nothing is stored.
    pub async fn find_pages(&self, user_token: &str) -> AppResult<InstagramPages> {
        let token = token_shape(user_token)?;
        let (mut pages, warning) = self.usable_pages(token).await?;
        pages.sort_by_key(|p| p.instagram_business_account.is_none());
        Ok(InstagramPages {
            pages: pages
                .into_iter()
                .map(|p| InstagramPageChoice {
                    page_id: p.id,
                    page_name: p.name,
                    instagram_username: p.instagram_business_account.map(|i| i.username),
                })
                .collect(),
            warning,
        })
    }

    /// Connects (or, with `reconnect`, reconnects) the Instagram account linked to a Page,
    /// with a pasted long-lived user token. Without `page_id` the only Page with a linked
    /// Instagram account is used.
    pub async fn connect(
        &self,
        user_token: &str,
        page_id: Option<&str>,
        reconnect: Option<AccountId>,
    ) -> AppResult<AccountView> {
        let token = token_shape(user_token)?;
        let (pages, _) = self.usable_pages(token).await?;
        let linked: Vec<&FbPage> = pages
            .iter()
            .filter(|p| p.instagram_business_account.is_some())
            .collect();
        let page = match page_id.map(str::trim).filter(|p| !p.is_empty()) {
            Some(id) => {
                let page = pages.iter().find(|p| p.id == id).ok_or_else(|| {
                    AppError::Invalid("That Page isn't among the token's Pages any more.".into())
                })?;
                if page.instagram_business_account.is_none() {
                    return Err(AppError::Invalid(format!(
                        "{} has no linked Instagram account. Link it in the Page's settings → \
                         Linked accounts → Instagram, or choose another Page.",
                        page.name
                    )));
                }
                page
            }
            None => match linked[..] {
                [only] => only,
                _ => {
                    let names: Vec<&str> = linked.iter().map(|p| p.name.as_str()).collect();
                    return Err(AppError::Invalid(format!(
                        "Choose which Page to connect: {}.",
                        join_names(&names)
                    )));
                }
            },
        };
        let ig = page
            .instagram_business_account
            .clone()
            .expect("a linked Instagram account");
        if page.access_token.is_empty() {
            return Err(AppError::Invalid(format!(
                "Meta didn't return {}'s token. Make sure you are an admin of the Page and \
                 that the token includes pages_show_list. {TOKEN_HELP}",
                page.name
            )));
        }
        let grant = Grant {
            login: InstagramLogin::FacebookLogin,
            access_token: page.access_token.clone(),
            user_id: ig.id.clone(),
            username: ig.username.clone(),
            page_id: Some(page.id.clone()),
            page_name: Some(page.name.clone()),
        };
        self.save_account(grant, reconnect).await
    }

    /// Connects an account with an Instagram Login token (`IGAA…`). Instagram doesn't let
    /// such tokens upload a file, so the account uses export packages.
    pub async fn connect_instagram_login(
        &self,
        access_token: &str,
        reconnect: Option<AccountId>,
    ) -> AppResult<AccountView> {
        let token = token_shape(access_token)?;
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
        let grant = Grant {
            login: InstagramLogin::InstagramLogin,
            access_token: token.to_owned(),
            user_id,
            username: profile.username,
            page_id: None,
            page_name: None,
        };
        self.save_account(grant, reconnect).await
    }

    /// Stores `grant` on the account it belongs to: `reconnect`, else the destination this
    /// Instagram account already has (matched by id, or by username for an account first
    /// connected with Instagram login), else a new one.
    async fn save_account(
        &self,
        grant: Grant,
        reconnect: Option<AccountId>,
    ) -> AppResult<AccountView> {
        let same = |g: &Grant| {
            g.user_id == grant.user_id
                || (!g.username.is_empty() && g.username.eq_ignore_ascii_case(&grant.username))
        };
        let mut existing = None;
        for a in self.app.store.accounts().list().await? {
            if a.platform != Platform::InstagramReels {
                continue;
            }
            let previous = self.tokens.grant(a.id).map_err(credential_error)?;
            if previous.as_ref().is_some_and(same) {
                existing = Some(a);
                break;
            }
        }
        let display = format!("@{}", grant.username);
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
                if previous.as_ref().is_some_and(|g| !same(g)) {
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
        let login = grant.login;
        self.tokens
            .save_grant(account.id, &grant)
            .map_err(credential_error)?;
        account.display_name = display;
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
                needs_reconnect: false,
                last_error: None,
            },
        );
        self.save_states(&states).await?;
        info!(account = %account.id, ?login, "Instagram account connected");
        if login == InstagramLogin::FacebookLogin {
            self.resume_waiting(account.id).await?;
        }
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
    /// connected again. (To revoke the app's access entirely, remove it in Facebook's
    /// settings under Business integrations.)
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

    /// Moves accounts connected with Instagram login before Facebook login existed to export
    /// packages once at start. (Page tokens don't expire, so nothing needs renewing.)
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        tokio::select! {
            () = shutdown.cancelled() => {}
            result = self.migrate() => {
                if let Err(e) = result {
                    warn!(error = %e, "could not check Instagram connections");
                }
            }
        }
    }
}

// --- cloud runner ----------------------------------------------------------------------------

/// The account's Page token for the cloud runner.
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
        login: g.login,
        expires_at: None,
    }))
}

/// The runner doesn't change Instagram credentials (Page tokens aren't renewed), so the
/// desktop's copy stays as it is.
pub(crate) fn import_cloud_credential(
    _credentials: &dyn CredentialStore,
    _account: AccountId,
    credential: &CloudCredential,
) -> Result<(), String> {
    match credential {
        CloudCredential::InstagramReels { .. } => Ok(()),
        _ => Err("not an Instagram credential".into()),
    }
}
