//! Adapter sessions from the encrypted credentials, refreshed the way the desktop connectors
//! refresh them: Google and Snap access tokens from refresh tokens (keeping rotated refresh
//! tokens), Instagram Page tokens as they are (they don't expire), LinkedIn tokens until they
//! expire.
//! Access tokens stay in memory. A refused refresh is an [`AdapterError::Authentication`], which
//! pauses the publication until the desktop sends a new credential.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use shorts_adapters::AdapterError;
use shorts_adapters::google::{ClientConfig, OAuthClient, OAuthError};
use shorts_adapters::instagram::{IgSession, InstagramAuth};
use shorts_adapters::linkedin::{LiSession, LinkedInAuth};
use shorts_adapters::snapchat::{SnapAuth, SnapAuthError, SnapClient, SnapOAuth, SnapSession};
use shorts_adapters::youtube::AccessTokens;
use shorts_cloud::{CloudCredential, Secret};
use shorts_domain::AccountId;
use shorts_domain::instagram::InstagramLogin;
use shorts_scheduler::Clock;
use tracing::{info, warn};

use crate::db::{PgStore, StoredCredential};

/// Refresh an access token when it has less than this left.
const REFRESH_MARGIN: Duration = Duration::seconds(120);

#[derive(Clone)]
struct Cached {
    token: String,
    /// The channel, profile or page the token is for.
    subject: String,
    expires_at: DateTime<Utc>,
}

/// Every connector's token source on the runner.
pub struct RunnerTokens {
    store: PgStore,
    clock: Arc<dyn Clock>,
    google: OAuthClient,
    snap: SnapOAuth,
    cache: Mutex<HashMap<AccountId, Cached>>,
    refresh: tokio::sync::Mutex<()>,
}

fn transient(e: impl std::fmt::Display) -> AdapterError {
    AdapterError::Transient {
        message: format!("credential store: {e}"),
        retry_after: None,
    }
}

fn reconnect(message: impl Into<String>) -> AdapterError {
    AdapterError::Authentication {
        message: message.into(),
    }
}

impl RunnerTokens {
    pub fn new(
        store: PgStore,
        clock: Arc<dyn Clock>,
        google: OAuthClient,
        snap: SnapOAuth,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            clock,
            google,
            snap,
            cache: Mutex::new(HashMap::new()),
            refresh: tokio::sync::Mutex::new(()),
        })
    }

    /// Drops cached access tokens, e.g. after the desktop sent a new credential.
    pub fn forget(&self, account: AccountId) {
        self.cache
            .lock()
            .expect("token cache lock")
            .remove(&account);
    }

    fn cached(&self, account: AccountId) -> Option<Cached> {
        let now = self.clock.now();
        self.cache
            .lock()
            .expect("token cache lock")
            .get(&account)
            .filter(|c| c.expires_at - now > REFRESH_MARGIN)
            .cloned()
    }

    fn prime(&self, account: AccountId, token: &str, subject: &str, expires_in: i64) {
        let expires_at = self.clock.now() + Duration::seconds(expires_in.max(0));
        self.cache.lock().expect("token cache lock").insert(
            account,
            Cached {
                token: token.to_owned(),
                subject: subject.to_owned(),
                expires_at,
            },
        );
    }

    fn drop_token(&self, account: AccountId, token: &str) {
        let mut cache = self.cache.lock().expect("token cache lock");
        if cache.get(&account).is_some_and(|c| c.token == token) {
            cache.remove(&account);
        }
    }

    async fn stored(&self, account: AccountId) -> Result<StoredCredential, AdapterError> {
        self.store
            .credential(account)
            .await
            .map_err(transient)?
            .ok_or_else(|| {
                reconnect(
                    "the runner has no credential for this account; reconnect it on the desktop",
                )
            })
    }

    /// Keeps a refreshed credential. If the desktop replaced it meanwhile, its copy wins.
    async fn keep(&self, account: AccountId, read: u64, credential: &CloudCredential) {
        match self
            .store
            .save_rotated_credential(account, read, credential, self.clock.now())
            .await
        {
            Ok(Some(revision)) => {
                info!(account = %account, revision, "stored a refreshed credential")
            }
            Ok(None) => {}
            Err(e) => {
                warn!(account = %account, error = %e, "could not store a refreshed credential")
            }
        }
    }

    /// Instagram: the Page token as stored. An Instagram Login credential can't upload, so
    /// it asks for a reconnect with Facebook login.
    async fn instagram_session(&self, account: AccountId) -> Result<IgSession, AdapterError> {
        let stored = self.stored(account).await?;
        let CloudCredential::InstagramReels {
            access_token,
            user_id,
            login,
            expires_at,
        } = stored.credential
        else {
            return Err(reconnect("the stored credential is not an Instagram token"));
        };
        if login != InstagramLogin::FacebookLogin {
            return Err(reconnect(
                "the Instagram account is connected with Instagram login, which can't upload \
                 files; reconnect it with Facebook login on the desktop",
            ));
        }
        if expires_at.is_some_and(|t| t <= self.clock.now()) {
            return Err(reconnect(
                "the Instagram token ran out; reconnect the account on the desktop",
            ));
        }
        Ok(IgSession {
            access_token: access_token.expose().to_owned(),
            user_id,
        })
    }
}

#[async_trait]
impl AccessTokens for RunnerTokens {
    async fn access_token(&self, account: AccountId) -> Result<String, AdapterError> {
        if let Some(c) = self.cached(account) {
            return Ok(c.token);
        }
        let _guard = self.refresh.lock().await;
        if let Some(c) = self.cached(account) {
            return Ok(c.token);
        }
        let stored = self.stored(account).await?;
        let CloudCredential::YoutubeShorts {
            client_id,
            client_secret,
            refresh_token,
            channel_id,
        } = stored.credential.clone()
        else {
            return Err(reconnect("the stored credential is not a YouTube sign-in"));
        };
        let client = ClientConfig::new(&client_id, client_secret.expose())
            .map_err(|e| reconnect(format!("the Google OAuth client is not usable: {e}")))?;
        match self.google.refresh(&client, refresh_token.expose()).await {
            Ok(tokens) => {
                self.prime(
                    account,
                    &tokens.access_token,
                    &channel_id,
                    tokens.expires_in,
                );
                if let Some(rotated) = tokens.refresh_token.filter(|t| t != refresh_token.expose())
                {
                    let credential = CloudCredential::YoutubeShorts {
                        client_id,
                        client_secret,
                        refresh_token: Secret::new(rotated),
                        channel_id,
                    };
                    self.keep(account, stored.revision, &credential).await;
                }
                Ok(tokens.access_token)
            }
            Err(OAuthError::Reauthorize { message }) => Err(reconnect(format!(
                "Google ended the channel's sign-in ({message}); reconnect it on the desktop. \
                 While the consent screen is in Testing this happens every 7 days."
            ))),
            Err(OAuthError::BadClient { message }) => Err(reconnect(format!(
                "Google rejected the OAuth client ID or secret ({message})"
            ))),
            Err(e) => Err(AdapterError::Transient {
                message: e.to_string(),
                retry_after: None,
            }),
        }
    }

    async fn invalidate(&self, account: AccountId, token: &str) {
        self.drop_token(account, token);
    }
}

#[async_trait]
impl InstagramAuth for RunnerTokens {
    async fn session(&self, account: AccountId) -> Result<IgSession, AdapterError> {
        self.instagram_session(account).await
    }

    async fn invalidate(&self, _account: AccountId, _token: &str) {
        // Tokens are read from the database on every call; nothing is cached.
    }
}

#[async_trait]
impl LinkedInAuth for RunnerTokens {
    async fn session(&self, account: AccountId) -> Result<LiSession, AdapterError> {
        let stored = self.stored(account).await?;
        let CloudCredential::Linkedin {
            access_token,
            organization,
            expires_at,
        } = stored.credential
        else {
            return Err(reconnect("the stored credential is not a LinkedIn token"));
        };
        if expires_at <= self.clock.now() {
            return Err(reconnect(
                "the LinkedIn token ran out (tokens last 60 days); generate a new one and \
                 reconnect on the desktop",
            ));
        }
        Ok(LiSession {
            access_token: access_token.expose().to_owned(),
            author: organization,
        })
    }

    async fn invalidate(&self, _account: AccountId, _token: &str) {}
}

#[async_trait]
impl SnapAuth for RunnerTokens {
    async fn session(&self, account: AccountId) -> Result<SnapSession, AdapterError> {
        if let Some(c) = self.cached(account) {
            return Ok(SnapSession {
                access_token: c.token,
                profile_id: c.subject,
            });
        }
        let _guard = self.refresh.lock().await;
        if let Some(c) = self.cached(account) {
            return Ok(SnapSession {
                access_token: c.token,
                profile_id: c.subject,
            });
        }
        let stored = self.stored(account).await?;
        let CloudCredential::SnapchatSpotlight {
            client_id,
            client_secret,
            refresh_token,
            profile_id,
        } = stored.credential.clone()
        else {
            return Err(reconnect("the stored credential is not a Snapchat sign-in"));
        };
        let client = SnapClient::new(&client_id, client_secret.expose())
            .map_err(|e| reconnect(format!("the Snap OAuth client is not usable: {e}")))?;
        match self.snap.refresh(&client, refresh_token.expose()).await {
            Ok(tokens) => {
                if let Some(rotated) = tokens
                    .refresh_token
                    .filter(|t| !t.is_empty() && t != refresh_token.expose())
                {
                    let credential = CloudCredential::SnapchatSpotlight {
                        client_id,
                        client_secret,
                        refresh_token: Secret::new(rotated),
                        profile_id: profile_id.clone(),
                    };
                    self.keep(account, stored.revision, &credential).await;
                }
                self.prime(
                    account,
                    &tokens.access_token,
                    &profile_id,
                    tokens.expires_in,
                );
                Ok(SnapSession {
                    access_token: tokens.access_token,
                    profile_id,
                })
            }
            Err(SnapAuthError::Reauthorize { message }) => Err(reconnect(format!(
                "Snap ended the profile's sign-in ({message}); reconnect it on the desktop"
            ))),
            Err(SnapAuthError::BadClient { message }) => Err(reconnect(format!(
                "Snap rejected the OAuth client id or secret ({message})"
            ))),
            Err(SnapAuthError::Transient { message }) => Err(AdapterError::Transient {
                message,
                retry_after: None,
            }),
            Err(e) => Err(reconnect(e.to_string())),
        }
    }

    async fn invalidate(&self, account: AccountId, token: &str) {
        self.drop_token(account, token);
    }
}
