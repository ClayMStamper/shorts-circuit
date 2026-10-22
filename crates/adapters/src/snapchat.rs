//! Snapchat Spotlight through Snap's Public Profile API (`businessapi.snapchat.com`), plus the
//! OAuth calls it needs.
//!
//! - **Sign-in** is OAuth 2 with the owner's Snap OAuth app (client id and secret, created in
//!   Ads Manager) and the `snapchat-profile-api` scope. Access tokens last an hour; refresh
//!   tokens renew them with the client secret. Snap gives Public Profile API access to
//!   allowlisted apps only.
//! - **Upload** follows "Profile Asset Management": the video is encrypted with AES-256-CBC
//!   under a fresh key and IV, a media object is created with them (`POST
//!   /v1/public_profiles/{id}/media`), the ciphertext goes up in parts of up to 32 MB
//!   (`add_path`, `action=ADD`), then `finalize_path` (`action=FINALIZE`). The key, IV and
//!   part count are in the checkpoint, so a restart re-encrypts to the same bytes and continues
//!   with the next part. Media objects last 24 hours; an older checkpoint starts over.
//! - **Publish** is `POST /v1/public_profiles/{id}/spotlights` with the media id, a locale and
//!   the description. Snap reviews Spotlight submissions, so a new post starts as `SUBMITTED`.
//! - **Reconcile** lists the profile's Spotlight posts and looks for one made from this media;
//!   without a match it leaves the decision to the user rather than risk a second post.
//!
//! Tokens, the client secret and the encryption key are never logged.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use aes::Aes256;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use chrono::{DateTime, Utc};
use rand::RngCore;
use reqwest::header::RETRY_AFTER;
use reqwest::multipart::{Form, Part};
use reqwest::{RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, Platform, PublishAccess, PublishAttempt,
    ValidationIssue, snapchat,
};
use thiserror::Error;
use tracing::debug;

use crate::contract::{
    Adapter, AdapterError, Metric, Processing, PublishedPost, Reconciliation, Submission,
    UploadProgress,
};
use crate::youtube::Now;

pub const API_BASE: &str = "https://businessapi.snapchat.com";
pub const AUTHORIZE_URL: &str = "https://accounts.snapchat.com/login/oauth2/authorize";
pub const TOKEN_URL: &str = "https://accounts.snapchat.com/login/oauth2/access_token";
pub const SCOPE: &str = "snapchat-profile-api";
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------------------------
// OAuth

/// The owner's Snap OAuth app.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapClient {
    pub client_id: String,
    pub client_secret: String,
}

impl fmt::Debug for SnapClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapClient").finish_non_exhaustive()
    }
}

impl SnapClient {
    /// Checks the shape of what was pasted (not whether Snap accepts it).
    pub fn new(client_id: &str, client_secret: &str) -> Result<Self, String> {
        let client_id = client_id.trim();
        let client_secret = client_secret.trim();
        if client_id.len() < 8 || client_id.chars().any(char::is_whitespace) {
            return Err("paste the OAuth client id from Ads Manager → Business Details".into());
        }
        if client_secret.len() < 8 || client_secret.chars().any(char::is_whitespace) {
            return Err("paste the OAuth client secret shown when the app was created".into());
        }
        Ok(Self {
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapEndpoints {
    pub authorize: Url,
    pub token: Url,
}

impl Default for SnapEndpoints {
    fn default() -> Self {
        Self {
            authorize: Url::parse(AUTHORIZE_URL).expect("valid URL"),
            token: Url::parse(TOKEN_URL).expect("valid URL"),
        }
    }
}

/// Snap's token response.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct SnapTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Seconds.
    #[serde(default)]
    pub expires_in: i64,
}

impl fmt::Debug for SnapTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapTokens")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SnapAuthError {
    /// The refresh token expired or was revoked: sign in again.
    #[error("Snap needs you to sign in again: {message}")]
    Reauthorize { message: String },
    #[error("Snap rejected the OAuth client id or secret: {message}")]
    BadClient { message: String },
    #[error("temporary problem reaching Snap: {message}")]
    Transient { message: String },
    #[error("Snap rejected the sign-in ({code}): {message}")]
    Rejected { code: String, message: String },
}

#[derive(Deserialize)]
struct TokenErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// Talks to Snap's OAuth endpoints.
#[derive(Debug, Clone)]
pub struct SnapOAuth {
    http: reqwest::Client,
    endpoints: SnapEndpoints,
}

impl SnapOAuth {
    pub fn new(http: reqwest::Client, endpoints: SnapEndpoints) -> Self {
        Self { http, endpoints }
    }

    pub fn authorization_url(&self, client: &SnapClient, redirect_uri: &str, state: &str) -> Url {
        let mut url = self.endpoints.authorize.clone();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", SCOPE)
            .append_pair("state", state);
        url
    }

    pub async fn exchange_code(
        &self,
        client: &SnapClient,
        code: &str,
        redirect_uri: &str,
    ) -> Result<SnapTokens, SnapAuthError> {
        self.token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &client.client_id),
            ("client_secret", &client.client_secret),
        ])
        .await
    }

    pub async fn refresh(
        &self,
        client: &SnapClient,
        refresh_token: &str,
    ) -> Result<SnapTokens, SnapAuthError> {
        self.token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &client.client_id),
            ("client_secret", &client.client_secret),
        ])
        .await
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<SnapTokens, SnapAuthError> {
        let response = self
            .http
            .post(self.endpoints.token.clone())
            .form(form)
            .send()
            .await
            .map_err(|e| SnapAuthError::Transient {
                message: if e.is_timeout() {
                    "timed out".into()
                } else {
                    "could not reach Snap".into()
                },
            })?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.map_err(|_| SnapAuthError::Transient {
                message: "unexpected answer from Snap".into(),
            });
        }
        let body: Option<TokenErrorBody> = response.json().await.ok();
        let (code, message) = body
            .map(|b| (b.error, b.error_description))
            .unwrap_or_default();
        let message = if message.is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            message
        };
        Err(match code.as_str() {
            "invalid_grant" => SnapAuthError::Reauthorize { message },
            "invalid_client" | "unauthorized_client" => SnapAuthError::BadClient { message },
            _ if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
                SnapAuthError::Transient { message }
            }
            "" => SnapAuthError::Rejected {
                code: format!("http_{}", status.as_u16()),
                message,
            },
            _ => SnapAuthError::Rejected { code, message },
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Publishing

/// A connected public profile's access token and id.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapSession {
    pub access_token: String,
    pub profile_id: String,
}

impl fmt::Debug for SnapSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapSession").finish_non_exhaustive()
    }
}

/// Tokens for connected public profiles.
#[async_trait]
pub trait SnapAuth: Send + Sync {
    /// [`AdapterError::Authentication`] means the profile must be reconnected.
    async fn session(&self, account: AccountId) -> Result<SnapSession, AdapterError>;
    /// Snap refused `token`; the next call should get a fresh one.
    async fn invalidate(&self, account: AccountId, token: &str);
}

/// The signed-in public profile, from `GET /v1/public_profiles/my_profile`.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapProfile {
    pub id: String,
    pub display_name: String,
    pub username: String,
}

impl fmt::Debug for SnapProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapProfile")
            .field("display_name", &self.display_name)
            .finish_non_exhaustive()
    }
}

/// An encrypted upload in progress.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapUpload {
    pub account_id: AccountId,
    pub media_id: String,
    /// Where parts go; named like the other adapters' so the pipeline records a new upload when
    /// it changes.
    pub upload_url: String,
    pub finalize_path: String,
    /// Base64 AES-256 key and IV. Re-encrypting with them reproduces the same ciphertext.
    pub key: String,
    pub iv: String,
    pub parts: usize,
    /// Parts Snap has accepted.
    pub sent: usize,
    pub total: u64,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for SnapUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapUpload")
            .field("media_id", &self.media_id)
            .field("parts", &self.parts)
            .field("sent", &self.sent)
            .finish_non_exhaustive()
    }
}

/// An uploaded media object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapMedia {
    /// The media id (the pipeline records it as the remote media id).
    pub id: String,
    pub account_id: AccountId,
    pub created_at: DateTime<Utc>,
}

/// A submitted Spotlight post.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapPost {
    pub id: String,
    pub account_id: AccountId,
}

#[derive(Debug, Clone)]
pub struct SnapConfig {
    /// `https://businessapi.snapchat.com`; tests point it at a fake.
    pub api_base: Url,
    pub locale: String,
    pub part_bytes: usize,
    pub part_timeout: Duration,
}

impl Default for SnapConfig {
    fn default() -> Self {
        Self {
            api_base: Url::parse(API_BASE).expect("valid URL"),
            locale: snapchat::DEFAULT_LOCALE.to_owned(),
            part_bytes: snapchat::PART_BYTES,
            part_timeout: Duration::from_secs(300),
        }
    }
}

/// AES-256-CBC with PKCS#7 padding, as `openssl enc -aes-256-cbc -nosalt` produces.
pub fn encrypt(plain: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Vec<u8> {
    cbc::Encryptor::<Aes256>::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plain)
}

fn decode_key<const N: usize>(b64: &str) -> Result<[u8; N], AdapterError> {
    BASE64
        .decode(b64)
        .ok()
        .and_then(|v| <[u8; N]>::try_from(v).ok())
        .ok_or_else(|| AdapterError::Permanent {
            message: "the stored upload key is unreadable; publish again".into(),
        })
}

/// Finds the first string value under `key` anywhere in a JSON document; Snap wraps the same
/// field differently across endpoints.
fn find_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    match v {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| map.values().find_map(|c| find_str(c, key))),
        Value::Array(items) => items.iter().find_map(|c| find_str(c, key)),
        _ => None,
    }
}

/// Spotlight entries of a listing: objects with an `id` that mention `media_id`.
fn spotlight_using(v: &Value, media_id: &str) -> Option<String> {
    match v {
        Value::Object(map) => {
            let mentions = map.values().any(|c| c.as_str() == Some(media_id));
            if mentions && let Some(id) = map.get("id").and_then(Value::as_str) {
                return Some(id.to_owned());
            }
            map.values().find_map(|c| spotlight_using(c, media_id))
        }
        Value::Array(items) => items.iter().find_map(|c| spotlight_using(c, media_id)),
        _ => None,
    }
}

fn transport(e: &reqwest::Error) -> AdapterError {
    if e.is_connect() {
        AdapterError::Transient {
            message: "could not connect to Snap".to_owned(),
            retry_after: None,
        }
    } else {
        AdapterError::ResponseLost {
            message: if e.is_timeout() {
                "Snap did not answer in time".to_owned()
            } else {
                "the connection to Snap broke before the answer arrived".to_owned()
            },
        }
    }
}

pub struct SnapAdapter {
    http: reqwest::Client,
    config: SnapConfig,
    auth: Arc<dyn SnapAuth>,
    now: Now,
}

impl fmt::Debug for SnapAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapAdapter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl SnapAdapter {
    pub fn new(
        http: reqwest::Client,
        config: SnapConfig,
        auth: Arc<dyn SnapAuth>,
        now: Now,
    ) -> Self {
        Self {
            http,
            config,
            auth,
            now,
        }
    }

    /// An API address from a path Snap returned (`add_path`, `finalize_path`), which may carry
    /// its own query string.
    fn api(&self, path: &str) -> Url {
        let mut url = self.config.api_base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let path = path.trim_start_matches('/');
        url.set_path(&format!("{prefix}/{path}"));
        url.set_query((!query.is_empty()).then_some(query));
        url
    }

    async fn error(&self, response: Response) -> AdapterError {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body: Value = response
            .text()
            .await
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let code = find_str(&body, "error_code").unwrap_or_default().to_owned();
        let message = find_str(&body, "display_message")
            .or_else(|| find_str(&body, "debug_message"))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        debug!(status = status.as_u16(), %code, "Snap error");
        match status {
            StatusCode::UNAUTHORIZED => AdapterError::Authentication {
                message: format!("Snap ended the sign-in ({message}); reconnect the profile"),
            },
            StatusCode::FORBIDDEN => AdapterError::Authentication {
                message: format!(
                    "Snap refused access ({message}). The OAuth app must be allowlisted for the \
                     Public Profile API and signed in by a profile admin"
                ),
            },
            StatusCode::TOO_MANY_REQUESTS => AdapterError::RateLimited {
                message: format!("Snap asked to slow down: {message}"),
                retry_after: retry_after.unwrap_or(RATE_LIMIT_PAUSE),
            },
            s if s.is_server_error() => AdapterError::Transient {
                message: format!("Snap: {message}"),
                retry_after,
            },
            _ if code == "MEDIA_EXPIRED" => AdapterError::Invalid {
                issues: vec![
                    "Snap's upload expired (they last 24 hours) before it was posted; publish \
                     again"
                        .into(),
                ],
            },
            s if s.is_client_error() => AdapterError::Invalid {
                issues: vec![format!("Snap rejected the Spotlight post: {message}")],
            },
            _ => AdapterError::Permanent {
                message: format!("Snap: {message}"),
            },
        }
    }

    /// Sends an authorized call, asking for a fresh token once if Snap refuses it.
    async fn send(
        &self,
        account: AccountId,
        build: impl Fn(&SnapSession) -> RequestBuilder,
    ) -> Result<(Response, SnapSession), AdapterError> {
        let mut retried = false;
        loop {
            let session = self.auth.session(account).await?;
            let response = build(&session)
                .bearer_auth(&session.access_token)
                .send()
                .await
                .map_err(|e| transport(&e))?;
            if response.status().is_success() {
                return Ok((response, session));
            }
            if response.status() == StatusCode::UNAUTHORIZED && !retried {
                self.auth.invalidate(account, &session.access_token).await;
                retried = true;
                continue;
            }
            return Err(self.error(response).await);
        }
    }

    async fn json(response: Response) -> Result<Value, AdapterError> {
        response.json().await.map_err(|e| transport(&e))
    }

    /// The public profile a fresh access token belongs to. Used when connecting.
    pub async fn my_profile(&self, access_token: &str) -> Result<SnapProfile, AdapterError> {
        let response = self
            .http
            .get(self.api("/v1/public_profiles/my_profile"))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| transport(&e))?;
        if !response.status().is_success() {
            return Err(self.error(response).await);
        }
        let body = Self::json(response).await?;
        let profile = body.get("public_profile").unwrap_or(&body);
        let text = |k: &str| {
            profile
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let id = text("id");
        if id.is_empty() {
            return Err(AdapterError::Invalid {
                issues: vec!["Snap didn't return a public profile for this sign-in".into()],
            });
        }
        Ok(SnapProfile {
            id,
            display_name: text("display_name"),
            username: text("snap_user_name"),
        })
    }

    async fn create_media(&self, s: &Submission<'_>) -> Result<SnapUpload, AdapterError> {
        if s.size_bytes == 0 {
            return Err(AdapterError::Invalid {
                issues: vec!["the rendered file is empty".to_owned()],
            });
        }
        let mut key = [0u8; 32];
        let mut iv = [0u8; 16];
        rand::rng().fill_bytes(&mut key);
        rand::rng().fill_bytes(&mut iv);
        let (key, iv) = (BASE64.encode(key), BASE64.encode(iv));
        let encrypted_len = (s.size_bytes / 16 + 1) * 16;
        let parts = usize::try_from(encrypted_len.div_ceil(self.config.part_bytes as u64))
            .unwrap_or(usize::MAX);
        if parts > snapchat::MAX_PARTS {
            return Err(AdapterError::Invalid {
                issues: vec!["the video is too large for a Spotlight upload".into()],
            });
        }
        let name = format!("shorts-circuit-{}", s.publication.id());
        let (response, _) = self
            .send(s.account.id, |session| {
                self.http
                    .post(self.api(&format!("/v1/public_profiles/{}/media", session.profile_id)))
                    .json(&json!({ "type": "VIDEO", "name": name, "key": key, "iv": iv }))
            })
            .await?;
        let body = Self::json(response).await?;
        let field = |k: &str| find_str(&body, k).map(str::to_owned);
        let (Some(media_id), Some(add_path), Some(finalize_path)) =
            (field("media_id"), field("add_path"), field("finalize_path"))
        else {
            return Err(AdapterError::Transient {
                message: "Snap didn't return where to upload the video".into(),
                retry_after: None,
            });
        };
        Ok(SnapUpload {
            account_id: s.account.id,
            media_id,
            upload_url: add_path,
            finalize_path,
            key,
            iv,
            parts,
            sent: 0,
            total: s.size_bytes,
            created_at: (self.now)(),
        })
    }

    async fn encrypted(
        &self,
        s: &Submission<'_>,
        cp: &SnapUpload,
    ) -> Result<Vec<u8>, AdapterError> {
        let plain = tokio::fs::read(s.media_path)
            .await
            .map_err(|e| AdapterError::Permanent {
                message: format!("could not read the rendered video: {e}"),
            })?;
        if plain.len() as u64 != cp.total {
            return Err(AdapterError::Permanent {
                message: "the rendered video changed during the upload; publish it again".into(),
            });
        }
        let key: [u8; 32] = decode_key(&cp.key)?;
        let iv: [u8; 16] = decode_key(&cp.iv)?;
        Ok(encrypt(&plain, &key, &iv))
    }

    fn post(&self, account: AccountId, id: String) -> PublishedPost<SnapPost> {
        PublishedPost {
            remote_id: id.clone(),
            url: None,
            note: Some(
                "Submitted to Spotlight. Snap reviews submissions before they go live; check \
                 your profile in the Snapchat app."
                    .into(),
            ),
            post: SnapPost {
                id,
                account_id: account,
            },
        }
    }
}

#[async_trait]
impl Adapter for SnapAdapter {
    type UploadCheckpoint = SnapUpload;
    type RemoteMedia = SnapMedia;
    type RemotePost = SnapPost;

    fn platform(&self) -> Platform {
        Platform::SnapchatSpotlight
    }

    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError> {
        let now = (self.now)();
        if let Err(AdapterError::Authentication { message }) = self.auth.session(account.id).await {
            return Ok(Capabilities::reconnect_required(
                &format!("Reconnect the profile to post automatically ({message})."),
                now,
            ));
        }
        Ok(Capabilities {
            publish_access: PublishAccess::Automatic,
            native_scheduling: false,
            analytics: false,
            manual_handoff: true,
            private_only: false,
            notes: vec![
                "Spotlight posts are reviewed by Snap before they go live.".to_owned(),
                "Videos must be MP4, 6 to 60 seconds, at least 540×960; descriptions up to 160 \
                 characters."
                    .to_owned(),
            ],
            checked_at: now,
        })
    }

    async fn validate(&self, s: &Submission<'_>) -> Result<Vec<ValidationIssue>, AdapterError> {
        let content = s.publication.content();
        let mut issues = Platform::SnapchatSpotlight.requirements().check(
            s.media,
            s.size_bytes,
            &content.caption,
            &content.hashtags,
        );
        let extension = shorts_domain::youtube::extension(&s.media_path.to_string_lossy());
        if !snapchat::FILE_EXTENSIONS.contains(&extension.as_str()) {
            issues.push(ValidationIssue::UnsupportedFormat { extension });
        }
        let (w, h) = s.media.display_dimensions();
        if w < snapchat::MIN_WIDTH || h < snapchat::MIN_HEIGHT {
            issues.push(ValidationIssue::ResolutionTooLow {
                width: w,
                height: h,
                min_width: snapchat::MIN_WIDTH,
                min_height: snapchat::MIN_HEIGHT,
            });
        }
        Ok(issues)
    }

    async fn begin_upload(&self, s: &Submission<'_>) -> Result<SnapUpload, AdapterError> {
        self.create_media(s).await
    }

    async fn upload_step(
        &self,
        s: &Submission<'_>,
        mut cp: SnapUpload,
    ) -> Result<UploadProgress<SnapUpload, SnapMedia>, AdapterError> {
        if cp.total != s.size_bytes {
            return Err(AdapterError::Permanent {
                message: "the rendered video changed during the upload; publish it again".into(),
            });
        }
        let age = (self.now)() - cp.created_at;
        if age > chrono::Duration::hours(snapchat::MEDIA_LIFETIME_HOURS - 1) {
            return Ok(UploadProgress::InProgress {
                checkpoint: self.create_media(s).await?,
                fraction: 0.0,
            });
        }
        if cp.sent < cp.parts {
            let data = self.encrypted(s, &cp).await?;
            let start = cp.sent * self.config.part_bytes;
            let end = (start + self.config.part_bytes).min(data.len());
            let chunk = data[start..end].to_vec();
            let part_number = (cp.sent + 1).to_string();
            let url = self.api(&cp.upload_url);
            self.send(cp.account_id, |_| {
                let form = Form::new()
                    .text("action", "ADD")
                    .text("part_number", part_number.clone())
                    .part(
                        "file",
                        Part::bytes(chunk.clone())
                            .file_name(format!("part-{part_number}"))
                            .mime_str("application/octet-stream")
                            .expect("valid MIME type"),
                    );
                self.http
                    .post(url.clone())
                    .timeout(self.config.part_timeout)
                    .multipart(form)
            })
            .await
            .map_err(|e| match e {
                // Re-sending a part is harmless.
                AdapterError::ResponseLost { message } => AdapterError::Transient {
                    message,
                    retry_after: None,
                },
                other => other,
            })?;
            cp.sent += 1;
            if cp.sent < cp.parts {
                #[allow(clippy::cast_precision_loss)]
                let fraction = cp.sent as f64 / cp.parts as f64;
                return Ok(UploadProgress::InProgress {
                    checkpoint: cp,
                    fraction,
                });
            }
        }
        let url = self.api(&cp.finalize_path);
        self.send(cp.account_id, |_| {
            self.http
                .post(url.clone())
                .multipart(Form::new().text("action", "FINALIZE"))
        })
        .await?;
        Ok(UploadProgress::Complete {
            media: SnapMedia {
                id: cp.media_id,
                account_id: cp.account_id,
                created_at: cp.created_at,
            },
        })
    }

    async fn check_processing(&self, _media: &SnapMedia) -> Result<Processing, AdapterError> {
        // Snap has no media status endpoint; problems surface when posting.
        Ok(Processing::Ready)
    }

    async fn publish(
        &self,
        s: &Submission<'_>,
        media: &SnapMedia,
    ) -> Result<PublishedPost<SnapPost>, AdapterError> {
        let content = s.publication.content();
        let description: String =
            shorts_domain::platform::full_caption(&content.caption, &content.hashtags)
                .replace('\n', " ")
                .chars()
                .take(snapchat::DESCRIPTION_MAX_CHARS)
                .collect();
        let body = json!({
            "media_id": media.id,
            "locale": self.config.locale,
            "description": description,
        });
        let (response, _) = self
            .send(media.account_id, |session| {
                self.http
                    .post(self.api(&format!(
                        "/v1/public_profiles/{}/spotlights",
                        session.profile_id
                    )))
                    .json(&body)
            })
            .await?;
        let answer = Self::json(response).await?;
        match find_str(&answer, "spotlight_id").or_else(|| find_str(&answer, "id")) {
            Some(id) => Ok(self.post(media.account_id, id.to_owned())),
            None => Err(AdapterError::ResponseLost {
                message: "Snap accepted the post without saying its id".into(),
            }),
        }
    }

    async fn reconcile(
        &self,
        attempt: &PublishAttempt,
        media: Option<&SnapMedia>,
    ) -> Result<Reconciliation<SnapPost>, AdapterError> {
        let (account, media_id) = match media {
            Some(m) => (m.account_id, m.id.clone()),
            None => {
                let Some(cp) = attempt
                    .upload_session
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<SnapUpload>(v.clone()).ok())
                else {
                    return Ok(Reconciliation::Unavailable {
                        reason: "no Snapchat upload was recorded for this attempt".into(),
                    });
                };
                (cp.account_id, cp.media_id)
            }
        };
        let (response, _) = self
            .send(account, |session| {
                self.http.get(self.api(&format!(
                    "/v1/public_profiles/{}/spotlights",
                    session.profile_id
                )))
            })
            .await?;
        let listing = Self::json(response).await?;
        Ok(match spotlight_using(&listing, &media_id) {
            Some(id) => Reconciliation::Found {
                post: self.post(account, id),
            },
            // The listing may not name the media, so "not listed" can't prove "not posted".
            None => Reconciliation::Unavailable {
                reason: "Snap's Spotlight list doesn't show which upload each post came from; \
                         check the profile in the Snapchat app"
                    .into(),
            },
        })
    }

    async fn fetch_metrics(&self, _post: &SnapPost) -> Result<Vec<Metric>, AdapterError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_matches_openssl() {
        // `printf 'hello spotlight' | openssl enc -aes-256-cbc -nosalt -K 00..1f -iv 00..0f`
        let key: [u8; 32] = std::array::from_fn(|i| i as u8);
        let iv: [u8; 16] = std::array::from_fn(|i| i as u8);
        let out = encrypt(b"hello spotlight", &key, &iv);
        let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "ed766df892fea054836ebea025a9e31c");
        assert_eq!(encrypt(b"hello spotlight", &key, &iv), out, "deterministic");
        let exact = encrypt(&[0u8; 32], &key, &iv);
        assert_eq!(exact.len(), 48, "a full block of padding is added");
    }

    #[test]
    fn nested_fields_are_found() {
        let v = json!({ "request_status": "SUCCESS", "media": [{ "sub": { "media_id": "m1" } }] });
        assert_eq!(find_str(&v, "media_id"), Some("m1"));
        assert_eq!(find_str(&v, "missing"), None);
        let list = json!({ "spotlights": [
            { "spotlight": { "id": "s0", "media_id": "other" } },
            { "spotlight": { "id": "s1", "media_id": "m1", "status": "SUBMITTED" } }
        ] });
        assert_eq!(spotlight_using(&list, "m1").as_deref(), Some("s1"));
        assert_eq!(spotlight_using(&list, "m9"), None);
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let c = SnapClient::new("client-id-123", "secret-value-456").unwrap();
        assert!(!format!("{c:?}").contains("secret-value"));
        let s = SnapSession {
            access_token: "token-abc".into(),
            profile_id: "p".into(),
        };
        assert!(!format!("{s:?}").contains("token-abc"));
        assert!(SnapClient::new("short", "secret-value-456").is_err());
    }
}
