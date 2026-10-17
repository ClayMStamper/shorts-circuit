//! LinkedIn Company Page connections: an access token the owner generates with the LinkedIn
//! developer portal's token generator (for their app with the Community Management API), and
//! the page to post as.
//!
//! Connecting by pasted token needs no redirect address, hosting or client secret. LinkedIn
//! issues 60-day tokens and only some partner programs may renew them programmatically, so the
//! app warns a week before a token runs out and asks for a new one when it has.
//!
//! Secrets live in the OS credential store only (`linkedin:<account id>`: the token and the
//! page's URN). SQLite holds the account, its capability record and a small status record per
//! page; views never include tokens.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use shorts_adapters::linkedin::{
    API_BASE, DEFAULT_API_VERSION, LiSession, LinkedInAdapter, LinkedInAuth, LinkedInConfig,
    Organization, default_name, organization_urn,
};
use shorts_adapters::{Adapter, AdapterError};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, Platform, PublicationEvent, PublicationState,
    linkedin,
};
use shorts_store::{PublicationFilter, Store};
use tracing::{info, warn};

use crate::app::App;
use crate::credentials::CredentialStore;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::views::AccountView;

const PAGES_KEY: &str = "linkedin_pages";
/// Show a warning when a token has less than this left.
const WARN_WITHIN: Duration = Duration::days(7);

pub const CONNECT_NEXT_STEP: &str = "Create a LinkedIn developer app linked to your Company Page, \
    get the Community Management API approved for it, generate an access token with the \
    token generator, and paste it here (see docs/human-scope.md).";

fn grant_key(account: AccountId) -> String {
    format!("linkedin:{account}")
}

fn credential_error(e: impl std::fmt::Display) -> AppError {
    AppError::Intake(format!("credential store: {e}"))
}

/// Where LinkedIn's API is and which version to ask for.
#[derive(Debug, Clone)]
pub struct LinkedInSetup {
    pub api_base: Url,
    pub api_version: String,
    pub request_timeout: StdDuration,
    pub part_timeout: StdDuration,
    pub poll_every: StdDuration,
}

impl Default for LinkedInSetup {
    fn default() -> Self {
        let config = LinkedInConfig::default();
        Self {
            api_base: Url::parse(API_BASE).expect("valid URL"),
            api_version: DEFAULT_API_VERSION.to_owned(),
            request_timeout: StdDuration::from_secs(60),
            part_timeout: config.part_timeout,
            poll_every: config.poll_every,
        }
    }
}

impl LinkedInSetup {
    /// The defaults, with the API version taken from `SHORTS_LINKEDIN_API_VERSION` (YYYYMM)
    /// when set.
    pub fn from_env() -> Self {
        let mut setup = Self::default();
        if let Ok(v) = std::env::var("SHORTS_LINKEDIN_API_VERSION") {
            let v = v.trim();
            if v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit()) {
                setup.api_version = v.to_owned();
            }
        }
        setup
    }
}

/// A page's token, kept in the credential store.
#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    access_token: String,
    organization: String,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// Non-secret per-page status, kept in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PageState {
    connected_at: DateTime<Utc>,
    token_expires_at: DateTime<Utc>,
    needs_reconnect: bool,
    last_error: Option<String>,
}

/// One LinkedIn destination as the Settings screen shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedInPage {
    pub account_id: String,
    pub display_name: String,
    pub connected: bool,
    pub needs_reconnect: bool,
    /// Less than a week left on the token.
    pub expires_soon: bool,
    pub last_error: Option<String>,
    pub notes: Vec<String>,
    pub connected_at: Option<DateTime<Utc>>,
    pub token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedInStatus {
    pub pages: Vec<LinkedInPage>,
    pub next_step: Option<String>,
}

/// Tokens from the credential store.
pub(crate) struct LinkedInTokens {
    credentials: Arc<dyn CredentialStore>,
    store: Store,
    clock: Arc<dyn shorts_scheduler::Clock>,
}

impl LinkedInTokens {
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

    async fn mark_reconnect(&self, account: AccountId, message: &str) {
        let result = async {
            let mut pages: HashMap<AccountId, PageState> = self
                .store
                .settings()
                .get(PAGES_KEY)
                .await?
                .unwrap_or_default();
            if let Some(state) = pages.get_mut(&account) {
                state.needs_reconnect = true;
                state.last_error = Some(message.to_owned());
                self.store.settings().set(PAGES_KEY, &pages).await?;
            }
            Ok::<_, shorts_store::StoreError>(())
        }
        .await;
        if let Err(e) = result {
            warn!(error = %e, "could not record that a LinkedIn page needs reconnecting");
        }
    }
}

#[async_trait]
impl LinkedInAuth for LinkedInTokens {
    async fn session(&self, account: AccountId) -> Result<LiSession, AdapterError> {
        let grant = self
            .grant(account)
            .map_err(|e| AdapterError::Transient {
                message: format!("credential store: {e}"),
                retry_after: None,
            })?
            .ok_or_else(|| AdapterError::Authentication {
                message: "the page is not connected; connect it in Settings".to_owned(),
            })?;
        if grant.expires_at <= self.clock.now() {
            let message = "the LinkedIn token ran out (tokens last 60 days); generate a new one \
                           with the developer portal's token generator and reconnect"
                .to_owned();
            self.mark_reconnect(account, &message).await;
            return Err(AdapterError::Authentication { message });
        }
        Ok(LiSession {
            access_token: grant.access_token,
            author: grant.organization,
        })
    }

    async fn invalidate(&self, account: AccountId, _token: &str) {
        self.mark_reconnect(
            account,
            "LinkedIn refused the saved token; generate a new one and reconnect",
        )
        .await;
    }
}

/// The LinkedIn connector.
pub struct LinkedInConnector {
    app: Arc<App>,
    adapter: Arc<LinkedInAdapter>,
    tokens: Arc<LinkedInTokens>,
    credentials: Arc<dyn CredentialStore>,
}

fn check_token(token: &str) -> AppResult<&str> {
    let token = token.trim();
    if token.len() < 20 || token.chars().any(char::is_whitespace) {
        return Err(AppError::Invalid(
            "paste the whole access token from LinkedIn's token generator".into(),
        ));
    }
    Ok(token)
}

fn refused(e: AdapterError) -> AppError {
    match e {
        AdapterError::Authentication { .. } => AppError::Invalid(
            "LinkedIn didn't accept that token. Generate a new one with the developer portal's \
             token generator and paste it again."
                .into(),
        ),
        other => AppError::Adapter(other),
    }
}

impl LinkedInConnector {
    /// Builds the connector and registers its adapter with the app.
    pub fn new(
        app: Arc<App>,
        setup: LinkedInSetup,
        credentials: Arc<dyn CredentialStore>,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(setup.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        let clock = app.clock();
        let tokens = Arc::new(LinkedInTokens {
            credentials: credentials.clone(),
            store: app.store.clone(),
            clock: clock.clone(),
        });
        let adapter = Arc::new(LinkedInAdapter::new(
            http,
            LinkedInConfig {
                api_base: setup.api_base,
                api_version: setup.api_version,
                poll_every: setup.poll_every,
                part_timeout: setup.part_timeout,
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

    async fn states(&self) -> AppResult<HashMap<AccountId, PageState>> {
        Ok(self
            .app
            .store
            .settings()
            .get(PAGES_KEY)
            .await?
            .unwrap_or_default())
    }

    async fn save_states(&self, states: &HashMap<AccountId, PageState>) -> AppResult<()> {
        self.app.store.settings().set(PAGES_KEY, states).await?;
        Ok(())
    }

    pub async fn status(&self) -> AppResult<LinkedInStatus> {
        let states = self.states().await?;
        let now = self.app.now();
        let mut pages = Vec::new();
        for account in self.app.store.accounts().list().await? {
            if account.platform != Platform::Linkedin {
                continue;
            }
            let state = states.get(&account.id);
            let connected = account.credential_ref.is_some()
                && self
                    .tokens
                    .grant(account.id)
                    .map_err(credential_error)?
                    .is_some();
            let expires = state.map(|s| s.token_expires_at);
            let expired = expires.is_some_and(|e| e <= now);
            pages.push(LinkedInPage {
                account_id: account.id.to_string(),
                display_name: account.display_name.clone(),
                connected,
                needs_reconnect: connected && (expired || state.is_some_and(|s| s.needs_reconnect)),
                expires_soon: connected
                    && !expired
                    && expires.is_some_and(|e| e - now < WARN_WITHIN),
                last_error: if expired && state.is_some_and(|s| s.last_error.is_none()) {
                    Some("The token ran out; generate a new one and reconnect.".to_owned())
                } else {
                    state.and_then(|s| s.last_error.clone())
                },
                notes: account.capabilities.notes.clone(),
                connected_at: state.map(|s| s.connected_at),
                token_expires_at: expires,
            });
        }
        let next_step = (!pages.iter().any(|p| p.connected)).then(|| CONNECT_NEXT_STEP.to_owned());
        Ok(LinkedInStatus { pages, next_step })
    }

    /// The Company Pages a pasted token's member administers, to choose from. Needs the token
    /// to include `rw_organization_admin`; otherwise the owner enters the page id.
    pub async fn find_pages(&self, access_token: &str) -> AppResult<Vec<Organization>> {
        let token = check_token(access_token)?;
        match self.adapter.organizations(token).await {
            Ok(pages) => Ok(pages),
            Err(AdapterError::Authentication { message }) if message.contains("permission") => {
                Err(AppError::Invalid(
                    "LinkedIn didn't list your pages for this token (it needs \
                     rw_organization_admin). Enter the page's id instead: it's the number in \
                     your page's admin address, linkedin.com/company/<id>/admin."
                        .into(),
                ))
            }
            Err(e) => Err(refused(e)),
        }
    }

    /// Connects (or, with `reconnect`, reconnects) a Company Page using a pasted token.
    /// `page` is the page's URN, numeric id, or admin address.
    pub async fn connect(
        &self,
        access_token: &str,
        page: &str,
        reconnect: Option<AccountId>,
    ) -> AppResult<AccountView> {
        let token = check_token(access_token)?;
        let urn = organization_urn(page).ok_or_else(|| {
            AppError::Invalid(
                "enter the page's numeric id (the number in linkedin.com/company/<id>/admin)"
                    .into(),
            )
        })?;
        let name = self
            .adapter
            .organization_name(token, &urn)
            .await
            .map_err(refused)?
            .unwrap_or_else(|| default_name(&urn));

        let accounts = self.app.store.accounts().list().await?;
        let mut existing = None;
        for a in accounts.iter().filter(|a| a.platform == Platform::Linkedin) {
            let grant = self.tokens.grant(a.id).map_err(credential_error)?;
            if grant.is_some_and(|g| g.organization == urn) {
                existing = Some(a.clone());
                break;
            }
        }
        let target = match (reconnect, existing) {
            (Some(id), Some(found)) if found.id != id => {
                return Err(AppError::Invalid(format!(
                    "{name} is already connected as \"{}\". Reconnect that page instead.",
                    found.display_name
                )));
            }
            (Some(id), _) => {
                let account = self.app.store.accounts().require(id).await?;
                if account.platform != Platform::Linkedin {
                    return Err(AppError::Invalid("that is not a LinkedIn account".into()));
                }
                let previous = self.tokens.grant(id).map_err(credential_error)?;
                if previous.is_some_and(|g| g.organization != urn) {
                    return Err(AppError::Invalid(format!(
                        "{name} is a different page from \"{}\". Connect it separately.",
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
            None => ConnectedAccount::manual(Platform::Linkedin, &name, now),
        };
        let is_new = self.app.store.accounts().get(account.id).await?.is_none();
        let grant = Grant {
            access_token: token.to_owned(),
            organization: urn,
            issued_at: now,
            expires_at: now + Duration::days(linkedin::TOKEN_LIFETIME_DAYS),
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
            PageState {
                connected_at: now,
                token_expires_at: grant.expires_at,
                needs_reconnect: false,
                last_error: None,
            },
        );
        self.save_states(&states).await?;
        info!(account = %account.id, "LinkedIn page connected");
        self.resume_waiting(account.id).await?;
        self.app.emit(AppEvent::LinkedInChanged);
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

    /// Forgets the page's token. The destination stays, using export packages until it is
    /// connected again.
    pub async fn disconnect(&self, account_id: AccountId) -> AppResult<LinkedInStatus> {
        let mut account = self.app.store.accounts().require(account_id).await?;
        if account.platform != Platform::Linkedin {
            return Err(AppError::Invalid("that is not a LinkedIn account".into()));
        }
        self.credentials
            .delete(&grant_key(account_id))
            .map_err(credential_error)?;
        account.capabilities = Capabilities::reconnect_required(
            "Disconnected. Connect the page again to post automatically; until then its \
             publications use export packages.",
            self.app.now(),
        );
        account.credential_ref = None;
        account.updated_at = self.app.now();
        self.app.store.accounts().update(&account).await?;
        let mut states = self.states().await?;
        states.remove(&account_id);
        self.save_states(&states).await?;
        self.app.emit(AppEvent::LinkedInChanged);
        self.status().await
    }
}
