//! Instagram Reels through the Instagram API with Facebook Login (`graph.facebook.com`).
//!
//! The account is an Instagram professional account linked to a Facebook Page; calls use the
//! Page's access token and the account's `instagram_business_account` id. (Uploading the file
//! itself is only offered to apps with Facebook Login for Business: with Instagram Login,
//! Instagram asks for a public `video_url` instead. [`InstagramAdapter::profile`] still reads
//! an Instagram Login account, for the export-only fallback.)
//!
//! - **Upload** creates a Reels media container with `upload_type=resumable` (the caption and
//!   `share_to_feed` go in with it), then sends the rendered file to the container's `rupload`
//!   address. Before sending, the container's `video_status.uploading_phase.bytes_transferred`
//!   says how much already arrived, so an interrupted upload continues from there instead of
//!   from byte 0 (and one that fully arrived isn't sent again). Containers expire after 24
//!   hours, so an older checkpoint opens a fresh container.
//! - **Processing** polls the container's `status_code` until it is `FINISHED`.
//! - **Publish** checks the account's 24-hour publishing limit, then calls `media_publish` and
//!   reads the new reel's permalink.
//! - **Reconcile** (after a lost publish answer) reads the container: `PUBLISHED` means the reel
//!   exists, and it is found among the account's recent media by its caption; `FINISHED` means
//!   nothing was published.
//!
//! Tokens come from an [`InstagramAuth`] source (the desktop keeps the Page token per account).
//! They go in `Authorization` headers and are never logged; the account's Instagram user id is
//! kept out of logs too.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Utc};
use reqwest::header::AUTHORIZATION;
use reqwest::{RequestBuilder, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shorts_domain::platform::full_caption;
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, Platform, PublishAccess, PublishAttempt,
    ValidationIssue, instagram,
};
use tracing::{debug, warn};

use crate::contract::{
    Adapter, AdapterError, Metric, Processing, PublishedPost, Reconciliation, Submission,
    UploadProgress,
};
use crate::youtube::Now;

pub const GRAPH_BASE: &str = "https://graph.facebook.com";
/// The Instagram Login host, used only to identify an account connected that way.
pub const INSTAGRAM_LOGIN_BASE: &str = "https://graph.instagram.com";
pub const UPLOAD_BASE: &str = "https://rupload.facebook.com";
/// Graph API version used for publishing calls. Meta supports each version for about two
/// years; `SHORTS_INSTAGRAM_API_VERSION` overrides it.
pub const DEFAULT_API_VERSION: &str = "v24.0";
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(15 * 60);
const PUBLISH_LIMIT_PAUSE: Duration = Duration::from_secs(60 * 60);
/// Containers expire 24 hours after they are created; start a new one a little earlier.
const CONTAINER_LIFETIME: chrono::Duration = chrono::Duration::hours(23);
/// Recent media searched when reconciling.
const RECENT_MEDIA: u32 = 25;
/// How much earlier than the upload a matching reel may be stamped (clock differences).
const MATCH_SLACK: chrono::Duration = chrono::Duration::minutes(10);

const CONNECT_NOTE: &str = "Connected through the Facebook Page with Facebook login. The Page's \
    token doesn't expire; if Meta ends it (password change, removed app or permissions), \
    reconnect with a new token.";

/// A connected account's Page token and Instagram user id.
#[derive(Clone, PartialEq, Eq)]
pub struct IgSession {
    /// The linked Facebook Page's access token.
    pub access_token: String,
    /// The professional account's Instagram user id (`instagram_business_account.id`).
    pub user_id: String,
}

impl fmt::Debug for IgSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IgSession").finish_non_exhaustive()
    }
}

/// Tokens for connected Instagram accounts.
#[async_trait]
pub trait InstagramAuth: Send + Sync {
    /// The account's current session. [`AdapterError::Authentication`] means it must be
    /// reconnected.
    async fn session(&self, account: AccountId) -> Result<IgSession, AdapterError>;
    /// Instagram refused `token`; the source should re-read or renew it.
    async fn invalidate(&self, account: AccountId, token: &str);
}

/// An account connected with Instagram Login, from `GET graph.instagram.com/me`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct IgProfile {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub account_type: String,
}

impl IgProfile {
    /// The id publishing calls use: `user_id`, or `id` when Instagram didn't send it.
    pub fn publishing_id(&self) -> &str {
        if self.user_id.is_empty() {
            &self.id
        } else {
            &self.user_id
        }
    }

    pub fn can_publish(&self) -> bool {
        instagram::PUBLISHING_ACCOUNT_TYPES.contains(&self.account_type.as_str())
    }
}

impl fmt::Debug for IgProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IgProfile")
            .field("username", &self.username)
            .field("account_type", &self.account_type)
            .finish_non_exhaustive()
    }
}

/// The Instagram professional account linked to a Page.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct LinkedInstagram {
    pub id: String,
    #[serde(default)]
    pub username: String,
}

/// A Facebook Page the user token can manage, from `GET /me/accounts`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct FbPage {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The Page's own token: it doesn't expire when it comes from a long-lived user token.
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub instagram_business_account: Option<LinkedInstagram>,
}

impl fmt::Debug for FbPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FbPage")
            .field("name", &self.name)
            .field(
                "instagram",
                &self
                    .instagram_business_account
                    .as_ref()
                    .map(|i| &i.username),
            )
            .finish_non_exhaustive()
    }
}

/// What Graph says about a pasted user token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCheck {
    /// Granted permissions.
    pub scopes: Vec<String>,
    /// `None` when the token never expires; also `None` with `expiry_known: false` when it
    /// couldn't be read (`/debug_token` refused, permissions read from `/me/permissions`).
    pub expires_at: Option<DateTime<Utc>>,
    pub expiry_known: bool,
}

#[derive(Debug, Default, Deserialize)]
struct DebugAnswer {
    #[serde(default)]
    data: DebugData,
}

#[derive(Debug, Default, Deserialize)]
struct DebugData {
    #[serde(default)]
    is_valid: Option<bool>,
    /// Unix seconds; 0 means never.
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    error: Option<GraphError>,
}

#[derive(Debug, Default, Deserialize)]
struct Permission {
    #[serde(default)]
    permission: String,
    #[serde(default)]
    status: String,
}

/// An open upload: the media container and where its bytes go.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstagramUpload {
    pub account_id: AccountId,
    pub container_id: String,
    /// The container's `rupload` address. Named like YouTube's so the pipeline notices a new
    /// container and records it on the attempt.
    pub upload_url: String,
    pub total: u64,
    /// Bytes Instagram had confirmed when this checkpoint was written.
    #[serde(default)]
    pub offset: u64,
    /// SHA-256 of the caption, to recognise the reel among recent media.
    pub caption_digest: String,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for InstagramUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstagramUpload")
            .field("container_id", &self.container_id)
            .field("total", &self.total)
            .field("offset", &self.offset)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

/// An uploaded container waiting to be published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstagramMedia {
    /// The container id (the pipeline records it as the remote media id).
    pub id: String,
    pub account_id: AccountId,
    pub caption_digest: String,
    pub uploaded_at: DateTime<Utc>,
}

/// A published reel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstagramPost {
    /// The Instagram media id.
    pub id: String,
    pub account_id: AccountId,
    pub permalink: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstagramConfig {
    /// `https://graph.facebook.com`; tests point it at a fake.
    pub graph_base: Url,
    /// `https://graph.instagram.com`, for accounts connected with Instagram Login.
    pub instagram_login_base: Url,
    /// Used when a container doesn't name its upload address.
    pub upload_base: Url,
    pub api_version: String,
    pub poll_every: Duration,
    /// Shortest time allowed for sending the file.
    pub upload_timeout: Duration,
    /// Slowest uplink the upload timeout allows for, in bytes per second.
    pub slowest_uplink: u64,
}

impl Default for InstagramConfig {
    fn default() -> Self {
        Self {
            graph_base: Url::parse(GRAPH_BASE).expect("valid URL"),
            instagram_login_base: Url::parse(INSTAGRAM_LOGIN_BASE).expect("valid URL"),
            upload_base: Url::parse(UPLOAD_BASE).expect("valid URL"),
            api_version: DEFAULT_API_VERSION.to_owned(),
            poll_every: Duration::from_secs(10),
            upload_timeout: Duration::from_secs(300),
            // ~0.5 Mbit/s.
            slowest_uplink: 64 * 1024,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: GraphError,
    /// The upload host (`rupload.facebook.com`) answers in this shape instead of `error`.
    #[serde(default)]
    debug_info: Option<DebugInfo>,
}

#[derive(Debug, Default, Deserialize)]
struct DebugInfo {
    #[serde(default)]
    retriable: bool,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    /// Plain text, or a JSON document such as `{"success":false,"error":{"message":…}}`.
    #[serde(default)]
    message: Option<String>,
}

impl DebugInfo {
    fn into_graph_error(self) -> GraphError {
        let message = self.message.unwrap_or_default();
        let inner = serde_json::from_str::<serde_json::Value>(&message)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .or_else(|| v.pointer("/message"))
                    .and_then(|m| m.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or(message);
        let message = match (self.kind.filter(|k| !k.trim().is_empty()), inner.trim()) {
            (Some(kind), "") => kind,
            (Some(kind), text) => format!("{text} ({kind})"),
            (None, text) => text.to_owned(),
        };
        GraphError {
            message,
            is_transient: self.retriable,
            ..GraphError::default()
        }
    }
}

/// The start of an answer that isn't JSON, on one line, so the owner sees something.
fn excerpt(body: &str) -> String {
    let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct GraphError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: i64,
    #[serde(default)]
    error_subcode: Option<i64>,
    #[serde(default)]
    is_transient: bool,
    #[serde(default)]
    error_user_msg: Option<String>,
}

impl GraphError {
    fn is_empty(&self) -> bool {
        self.code == 0
            && self.message.trim().is_empty()
            && self
                .error_user_msg
                .as_deref()
                .is_none_or(|m| m.trim().is_empty())
    }

    fn is_auth(&self, status: StatusCode) -> bool {
        status == StatusCode::UNAUTHORIZED || matches!(self.code, 102 | 190 | 463 | 467)
    }

    /// Why Meta ended the sign-in, from code 190's subcode, with what to do.
    fn auth_reason(&self) -> &'static str {
        match self.error_subcode {
            Some(458) => "the app was removed from your Facebook account's settings",
            Some(459) => "Facebook wants you to log in at facebook.com to unlock the account",
            Some(460) => "the Facebook password changed",
            Some(463) => "the token expired (it wasn't extended to a long-lived one)",
            Some(464) => "the Facebook account isn't confirmed yet",
            Some(467) => "the token is no longer valid (you logged out or Meta reset it)",
            Some(492) => "you are no longer an admin of the Facebook Page",
            _ => "Meta no longer accepts the saved token",
        }
    }

    fn text(&self, status: StatusCode) -> String {
        self.error_user_msg
            .clone()
            .filter(|m| !m.trim().is_empty())
            .or_else(|| Some(self.message.clone()).filter(|m| !m.trim().is_empty()))
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()))
    }
}

#[derive(Debug, Deserialize)]
struct Created {
    id: String,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct UploadAnswer {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ContainerStatus {
    #[serde(default)]
    status_code: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct UploadStatus {
    #[serde(default)]
    video_status: Option<VideoStatus>,
}

#[derive(Debug, Default, Deserialize)]
struct VideoStatus {
    #[serde(default)]
    uploading_phase: Option<UploadingPhase>,
}

#[derive(Debug, Default, Deserialize)]
struct UploadingPhase {
    /// Meta sends it as a number; tolerate a string.
    #[serde(default, deserialize_with = "number_or_string")]
    bytes_transferred: Option<u64>,
}

fn number_or_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

#[derive(Debug, Deserialize)]
struct Published {
    id: String,
}

#[derive(Debug, Default, Deserialize)]
struct MediaItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    permalink: Option<String>,
    #[serde(default)]
    shortcode: Option<String>,
    #[serde(default)]
    like_count: Option<f64>,
    #[serde(default)]
    comments_count: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Debug, Default, Deserialize)]
struct PublishingLimit {
    #[serde(default)]
    quota_usage: u32,
    #[serde(default)]
    config: Option<LimitConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct LimitConfig {
    #[serde(default)]
    quota_total: u32,
}

/// SHA-256 of the trimmed caption, hex.
pub fn caption_digest(caption: &str) -> String {
    Sha256::digest(caption.trim().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Instagram stamps media like `2026-10-16T23:02:11+0000`.
fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .or_else(|_| DateTime::<FixedOffset>::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%z"))
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn transport(e: &reqwest::Error) -> AdapterError {
    if e.is_connect() {
        AdapterError::Transient {
            message: "could not connect to Instagram".to_owned(),
            retry_after: None,
        }
    } else {
        AdapterError::ResponseLost {
            message: if e.is_timeout() {
                "Instagram did not answer in time".to_owned()
            } else {
                "the connection to Instagram broke before the answer arrived".to_owned()
            },
        }
    }
}

async fn read_error(response: Response) -> (StatusCode, GraphError) {
    let status = response.status();
    let raw = response.text().await.unwrap_or_default();
    let Ok(body) = serde_json::from_str::<ErrorBody>(&raw) else {
        let message = excerpt(&raw);
        return (
            status,
            GraphError {
                message,
                ..GraphError::default()
            },
        );
    };
    match body.debug_info {
        Some(info) if body.error.is_empty() => (status, info.into_graph_error()),
        _ => (status, body.error),
    }
}

fn classify(status: StatusCode, e: &GraphError) -> AdapterError {
    let text = e.text(status);
    debug!(
        status = status.as_u16(),
        code = e.code,
        subcode = e.error_subcode,
        "Instagram error"
    );
    if e.is_auth(status) {
        return AdapterError::Authentication {
            message: format!(
                "Instagram ended the sign-in: {} ({text}); reconnect with a new token",
                e.auth_reason()
            ),
        };
    }
    match (e.code, e.error_subcode) {
        (10 | 200..=299, _) => AdapterError::Authentication {
            message: format!(
                "Instagram refused permission ({text}); generate a new token that includes \
                 instagram_basic, instagram_content_publish, pages_show_list and \
                 pages_read_engagement, and reconnect"
            ),
        },
        (_, Some(2_207_042)) => AdapterError::RateLimited {
            message: "the account reached Instagram's limit of API posts in 24 hours".to_owned(),
            retry_after: PUBLISH_LIMIT_PAUSE,
        },
        (4 | 17 | 32 | 613 | 80_002, _) => AdapterError::RateLimited {
            message: format!("Instagram asked to slow down: {text}"),
            retry_after: RATE_LIMIT_PAUSE,
        },
        _ if status == StatusCode::TOO_MANY_REQUESTS => AdapterError::RateLimited {
            message: format!("Instagram asked to slow down: {text}"),
            retry_after: RATE_LIMIT_PAUSE,
        },
        _ if e.is_transient || matches!(e.code, 1 | 2) || status.is_server_error() => {
            AdapterError::Transient {
                message: format!("Instagram: {text}"),
                retry_after: None,
            }
        }
        _ if status.is_client_error() => AdapterError::Invalid {
            issues: vec![format!("Instagram rejected the reel: {text}")],
        },
        _ => AdapterError::Permanent {
            message: format!("Instagram: {text}"),
        },
    }
}

fn versioned(base: &Url, version: &str, path: &str) -> Url {
    let mut url = base.clone();
    let prefix = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{prefix}/{version}{path}"));
    url
}

async fn read_json<T: DeserializeOwned>(
    sent: reqwest::Result<Response>,
) -> Result<T, AdapterError> {
    let response = sent.map_err(|e| transport(&e))?;
    if !response.status().is_success() {
        let (status, error) = read_error(response).await;
        return Err(classify(status, &error));
    }
    response.json().await.map_err(|e| transport(&e))
}

pub struct InstagramAdapter {
    http: reqwest::Client,
    config: InstagramConfig,
    auth: Arc<dyn InstagramAuth>,
    now: Now,
}

impl fmt::Debug for InstagramAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstagramAdapter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl InstagramAdapter {
    pub fn new(
        http: reqwest::Client,
        config: InstagramConfig,
        auth: Arc<dyn InstagramAuth>,
        now: Now,
    ) -> Self {
        Self {
            http,
            config,
            auth,
            now,
        }
    }

    fn graph(&self, path: &str) -> Url {
        versioned(&self.config.graph_base, &self.config.api_version, path)
    }

    fn default_upload_url(&self, container: &str) -> Url {
        let mut url = self.config.upload_base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!(
            "{prefix}/ig-api-upload/{}/{container}",
            self.config.api_version
        ));
        url
    }

    fn upload_timeout(&self, size: u64) -> Duration {
        let secs = size / self.config.slowest_uplink.max(1);
        self.config.upload_timeout.max(Duration::from_secs(secs))
    }

    /// Sends an authorized Graph call and reads its JSON answer, renewing the token once if
    /// Instagram refuses it.
    async fn call<T: DeserializeOwned>(
        &self,
        account: AccountId,
        build: impl Fn(&IgSession) -> RequestBuilder,
    ) -> Result<T, AdapterError> {
        let mut retried = false;
        loop {
            let session = self.auth.session(account).await?;
            let response = build(&session).send().await.map_err(|e| transport(&e))?;
            if response.status().is_success() {
                return response.json().await.map_err(|e| transport(&e));
            }
            let (status, error) = read_error(response).await;
            if error.is_auth(status) && !retried {
                self.auth.invalidate(account, &session.access_token).await;
                retried = true;
                continue;
            }
            return Err(classify(status, &error));
        }
    }

    /// Who an Instagram Login token belongs to (`graph.instagram.com/me`). Used only to
    /// connect an account in the export-only Instagram Login mode.
    pub async fn profile(&self, access_token: &str) -> Result<IgProfile, AdapterError> {
        let url = versioned(
            &self.config.instagram_login_base,
            &self.config.api_version,
            "/me",
        );
        read_json(
            self.http
                .get(url)
                .query(&[("fields", "user_id,username,account_type")])
                .bearer_auth(access_token)
                .send()
                .await,
        )
        .await
    }

    /// The Pages a pasted user token manages, with their tokens and linked Instagram accounts.
    pub async fn pages(&self, user_token: &str) -> Result<Vec<FbPage>, AdapterError> {
        let page: Page<FbPage> = read_json(
            self.http
                .get(self.graph("/me/accounts"))
                .query(&[
                    (
                        "fields",
                        "name,access_token,instagram_business_account{id,username}",
                    ),
                    ("limit", "100"),
                ])
                .bearer_auth(user_token)
                .send()
                .await,
        )
        .await?;
        Ok(page.data)
    }

    /// The pasted token's permissions and expiry. `/debug_token` accepts the token itself as
    /// the caller when its user is a developer of the app (the owner is its admin), so no app
    /// secret is needed; if Meta refuses that, the permissions come from `/me/permissions`
    /// and the expiry is unknown.
    pub async fn check_token(&self, user_token: &str) -> Result<TokenCheck, AdapterError> {
        let debug: Result<DebugAnswer, _> = read_json(
            self.http
                .get(self.graph("/debug_token"))
                .query(&[("input_token", user_token)])
                .bearer_auth(user_token)
                .send()
                .await,
        )
        .await;
        match debug {
            Ok(DebugAnswer { data }) if data.error.is_none() && data.is_valid != Some(false) => {
                let expires_at = data
                    .expires_at
                    .filter(|&t| t > 0)
                    .and_then(|t| DateTime::from_timestamp(t, 0));
                return Ok(TokenCheck {
                    scopes: data.scopes,
                    expires_at,
                    expiry_known: true,
                });
            }
            Ok(DebugAnswer { data }) => {
                let error = data.error.unwrap_or_default();
                return Err(AdapterError::Authentication {
                    message: format!("Meta says the token isn't valid: {}", error.auth_reason()),
                });
            }
            Err(e @ AdapterError::Authentication { .. }) => {
                // An invalid token fails here too; /me/permissions tells which it was.
                debug!(error = %e, "debug_token refused; reading /me/permissions");
            }
            Err(e) => debug!(error = %e, "debug_token unavailable; reading /me/permissions"),
        }
        let granted: Page<Permission> = read_json(
            self.http
                .get(self.graph("/me/permissions"))
                .bearer_auth(user_token)
                .send()
                .await,
        )
        .await?;
        Ok(TokenCheck {
            scopes: granted
                .data
                .into_iter()
                .filter(|p| p.status == "granted")
                .map(|p| p.permission)
                .collect(),
            expires_at: None,
            expiry_known: false,
        })
    }

    /// How many bytes of the container's file Instagram already has, from
    /// `video_status.uploading_phase.bytes_transferred`. Unknown (e.g. the field isn't
    /// offered) counts as none, so the upload starts from the beginning.
    async fn bytes_received(&self, cp: &InstagramUpload) -> Result<u64, AdapterError> {
        let answer: Result<UploadStatus, _> = self
            .call(cp.account_id, |session| {
                self.http
                    .get(self.graph(&format!("/{}", cp.container_id)))
                    .query(&[("fields", "video_status")])
                    .bearer_auth(&session.access_token)
            })
            .await;
        match answer {
            Ok(status) => Ok(status
                .video_status
                .and_then(|v| v.uploading_phase)
                .and_then(|u| u.bytes_transferred)
                .unwrap_or(0)
                .min(cp.total)),
            Err(e @ AdapterError::Authentication { .. }) => Err(e),
            Err(e) => {
                warn!(error = %e, "could not read how much of the upload arrived; sending it all");
                Ok(0)
            }
        }
    }

    async fn create_container(&self, s: &Submission<'_>) -> Result<InstagramUpload, AdapterError> {
        if s.size_bytes == 0 {
            return Err(AdapterError::Invalid {
                issues: vec!["the rendered file is empty".to_owned()],
            });
        }
        let content = s.publication.content();
        let caption = full_caption(&content.caption, &content.hashtags);
        let share_to_feed = if content.options.share_to_feed.unwrap_or(true) {
            "true"
        } else {
            "false"
        };
        let created: Created = self
            .call(s.account.id, |session| {
                self.http
                    .post(self.graph(&format!("/{}/media", session.user_id)))
                    .bearer_auth(&session.access_token)
                    .form(&[
                        ("media_type", "REELS"),
                        ("upload_type", "resumable"),
                        ("caption", caption.as_str()),
                        ("share_to_feed", share_to_feed),
                    ])
            })
            .await?;
        let upload_url = created
            .uri
            .as_deref()
            .and_then(|u| Url::parse(u).ok())
            .filter(|u| u.scheme() == "https" || u.scheme() == "http")
            .unwrap_or_else(|| self.default_upload_url(&created.id));
        Ok(InstagramUpload {
            account_id: s.account.id,
            container_id: created.id,
            upload_url: upload_url.into(),
            total: s.size_bytes,
            offset: 0,
            caption_digest: caption_digest(&caption),
            created_at: (self.now)(),
        })
    }

    async fn container(
        &self,
        account: AccountId,
        id: &str,
    ) -> Result<ContainerStatus, AdapterError> {
        self.call(account, |session| {
            self.http
                .get(self.graph(&format!("/{id}")))
                .query(&[("fields", "status_code,status")])
                .bearer_auth(&session.access_token)
        })
        .await
    }

    async fn media_item(
        &self,
        account: AccountId,
        id: &str,
        fields: &str,
    ) -> Result<MediaItem, AdapterError> {
        self.call(account, |session| {
            self.http
                .get(self.graph(&format!("/{id}")))
                .query(&[("fields", fields)])
                .bearer_auth(&session.access_token)
        })
        .await
    }

    /// Refuses to publish while the account's 24-hour API posting limit is used up. If the
    /// limit can't be read, publishing goes ahead and Instagram has the last word.
    async fn check_publishing_limit(&self, account: AccountId) -> Result<(), AdapterError> {
        let page: Result<Page<PublishingLimit>, _> = self
            .call(account, |session| {
                self.http
                    .get(self.graph(&format!("/{}/content_publishing_limit", session.user_id)))
                    .query(&[("fields", "quota_usage,config")])
                    .bearer_auth(&session.access_token)
            })
            .await;
        match page {
            Ok(page) => {
                if let Some(limit) = page.data.first() {
                    let total = limit
                        .config
                        .as_ref()
                        .map_or(instagram::POSTS_PER_DAY, |c| c.quota_total);
                    if total > 0 && limit.quota_usage >= total {
                        return Err(AdapterError::RateLimited {
                            message: format!(
                                "the account has published {} of its {total} API posts in the \
                                 last 24 hours; trying again later",
                                limit.quota_usage
                            ),
                            retry_after: PUBLISH_LIMIT_PAUSE,
                        });
                    }
                }
                Ok(())
            }
            Err(e @ AdapterError::Authentication { .. }) => Err(e),
            Err(e) => {
                warn!(error = %e, "could not read Instagram's publishing limit");
                Ok(())
            }
        }
    }

    fn post(
        &self,
        account: AccountId,
        id: String,
        item: Option<&MediaItem>,
    ) -> PublishedPost<InstagramPost> {
        let permalink = item.and_then(|i| {
            i.permalink
                .clone()
                .filter(|p| p.starts_with("https://"))
                .or_else(|| i.shortcode.as_deref().map(instagram::reel_url))
        });
        PublishedPost {
            remote_id: id.clone(),
            url: permalink.clone(),
            note: None,
            post: InstagramPost {
                id,
                account_id: account,
                permalink,
            },
        }
    }

    /// The reel among the account's recent media whose caption matches the upload.
    async fn find_published(
        &self,
        media: &InstagramMedia,
    ) -> Result<Option<MediaItem>, AdapterError> {
        let limit = RECENT_MEDIA.to_string();
        let page: Page<MediaItem> = self
            .call(media.account_id, |session| {
                self.http
                    .get(self.graph(&format!("/{}/media", session.user_id)))
                    .query(&[
                        ("fields", "id,caption,timestamp,permalink,shortcode"),
                        ("limit", limit.as_str()),
                    ])
                    .bearer_auth(&session.access_token)
            })
            .await?;
        let earliest = media.uploaded_at - MATCH_SLACK;
        Ok(page.data.into_iter().find(|item| {
            let caption = item.caption.as_deref().unwrap_or_default();
            let recent = item
                .timestamp
                .as_deref()
                .and_then(parse_timestamp)
                .is_none_or(|t| t >= earliest);
            recent && caption_digest(caption) == media.caption_digest
        }))
    }
}

#[async_trait]
impl Adapter for InstagramAdapter {
    type UploadCheckpoint = InstagramUpload;
    type RemoteMedia = InstagramMedia;
    type RemotePost = InstagramPost;

    fn platform(&self) -> Platform {
        Platform::InstagramReels
    }

    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError> {
        let now = (self.now)();
        if let Err(AdapterError::Authentication { message }) = self.auth.session(account.id).await {
            return Ok(Capabilities::reconnect_required(
                &format!("Reconnect the account to publish automatically ({message})."),
                now,
            ));
        }
        Ok(Capabilities {
            publish_access: PublishAccess::Automatic,
            native_scheduling: false,
            analytics: true,
            manual_handoff: true,
            private_only: false,
            notes: vec![
                format!(
                    "Instagram allows {} posts through the API per account in 24 hours.",
                    instagram::POSTS_PER_DAY
                ),
                "Reels are published to your followers and shared to the profile grid.".to_owned(),
                CONNECT_NOTE.to_owned(),
            ],
            checked_at: now,
        })
    }

    async fn validate(&self, s: &Submission<'_>) -> Result<Vec<ValidationIssue>, AdapterError> {
        let content = s.publication.content();
        let mut issues = Platform::InstagramReels.requirements().check(
            s.media,
            s.size_bytes,
            &content.caption,
            &content.hashtags,
        );
        let extension = shorts_domain::youtube::extension(&s.media_path.to_string_lossy());
        if !instagram::FILE_EXTENSIONS.contains(&extension.as_str()) {
            issues.push(ValidationIssue::UnsupportedFormat { extension });
        }
        let mentions = instagram::mention_count(&content.caption);
        if mentions > instagram::MAX_MENTIONS {
            issues.push(ValidationIssue::TooManyMentions {
                count: mentions,
                max: instagram::MAX_MENTIONS,
            });
        }
        Ok(issues)
    }

    async fn begin_upload(&self, s: &Submission<'_>) -> Result<InstagramUpload, AdapterError> {
        self.create_container(s).await
    }

    async fn upload_step(
        &self,
        s: &Submission<'_>,
        cp: InstagramUpload,
    ) -> Result<UploadProgress<InstagramUpload, InstagramMedia>, AdapterError> {
        if cp.total != s.size_bytes {
            return Err(AdapterError::Permanent {
                message: "the rendered video changed during the upload; publish it again".into(),
            });
        }
        if (self.now)() - cp.created_at > CONTAINER_LIFETIME {
            // The container is about to expire (or has); upload into a new one.
            return Ok(UploadProgress::InProgress {
                checkpoint: self.create_container(s).await?,
                fraction: 0.0,
            });
        }
        let bytes = tokio::fs::read(s.media_path)
            .await
            .map_err(|e| AdapterError::Permanent {
                message: format!("could not read the rendered video: {e}"),
            })?;
        if bytes.len() as u64 != cp.total {
            return Err(AdapterError::Permanent {
                message: "the rendered video changed during the upload; publish it again".into(),
            });
        }
        // Ask where an interrupted upload stopped; a fresh container has nothing yet.
        let offset = self.bytes_received(&cp).await?;
        let complete = |cp: InstagramUpload, now| UploadProgress::Complete {
            media: InstagramMedia {
                id: cp.container_id,
                account_id: cp.account_id,
                caption_digest: cp.caption_digest,
                uploaded_at: now,
            },
        };
        if offset >= cp.total {
            debug!("the whole file had already arrived");
            return Ok(complete(cp, (self.now)()));
        }
        if offset > 0 {
            debug!(offset, total = cp.total, "resuming the Instagram upload");
        }
        let start = usize::try_from(offset).unwrap_or(0);
        let session = self.auth.session(cp.account_id).await?;
        let sent = self
            .http
            .post(cp.upload_url.as_str())
            .header(AUTHORIZATION, format!("OAuth {}", session.access_token))
            .header("offset", offset.to_string())
            .header("file_size", cp.total.to_string())
            .timeout(self.upload_timeout(cp.total - offset))
            .body(bytes[start..].to_vec())
            .send()
            .await;
        let response = match sent {
            Ok(r) => r,
            // The next step asks Instagram how much arrived and continues from there.
            Err(e) => {
                return Err(AdapterError::Transient {
                    message: match transport(&e) {
                        AdapterError::Transient { message, .. }
                        | AdapterError::ResponseLost { message } => {
                            format!("{message}; the upload continues where it stopped")
                        }
                        other => other.to_string(),
                    },
                    retry_after: None,
                });
            }
        };
        if response.status().is_success() {
            let answer: UploadAnswer = response.json().await.unwrap_or_default();
            if answer.success == Some(false) {
                return Err(AdapterError::Transient {
                    message: format!(
                        "Instagram did not accept the upload ({}); it is sent again",
                        answer.message.unwrap_or_else(|| "no reason given".into())
                    ),
                    retry_after: None,
                });
            }
            return Ok(complete(cp, (self.now)()));
        }
        let (status, error) = read_error(response).await;
        if error.is_auth(status) {
            self.auth
                .invalidate(cp.account_id, &session.access_token)
                .await;
            // The next step reads the token again; a revoked one then asks for a reconnect.
            return Err(AdapterError::Transient {
                message: "Instagram refused the upload's token; checking the sign-in".into(),
                retry_after: None,
            });
        }
        Err(classify(status, &error))
    }

    async fn check_processing(&self, media: &InstagramMedia) -> Result<Processing, AdapterError> {
        let status = self.container(media.account_id, &media.id).await?;
        Ok(match status.status_code.as_str() {
            "FINISHED" | "PUBLISHED" => Processing::Ready,
            "ERROR" => Processing::Failed {
                message: format!(
                    "Instagram could not process the reel ({})",
                    status.status.as_deref().unwrap_or("no reason given")
                ),
            },
            "EXPIRED" => Processing::Failed {
                message: "Instagram's upload expired before it was published; publish it again"
                    .into(),
            },
            _ => Processing::Pending {
                poll_after: self.config.poll_every,
                detail: Some("Instagram is processing the reel".into()),
            },
        })
    }

    async fn publish(
        &self,
        _s: &Submission<'_>,
        media: &InstagramMedia,
    ) -> Result<PublishedPost<InstagramPost>, AdapterError> {
        self.check_publishing_limit(media.account_id).await?;
        let published: Published = self
            .call(media.account_id, |session| {
                self.http
                    .post(self.graph(&format!("/{}/media_publish", session.user_id)))
                    .bearer_auth(&session.access_token)
                    .form(&[("creation_id", media.id.as_str())])
            })
            .await?;
        // The reel exists now; a missing permalink must not turn that into a failure.
        let item = match self
            .media_item(media.account_id, &published.id, "permalink,shortcode")
            .await
        {
            Ok(item) => Some(item),
            Err(e) => {
                warn!(error = %e, "could not read the new reel's link");
                None
            }
        };
        Ok(self.post(media.account_id, published.id, item.as_ref()))
    }

    async fn reconcile(
        &self,
        attempt: &PublishAttempt,
        media: Option<&InstagramMedia>,
    ) -> Result<Reconciliation<InstagramPost>, AdapterError> {
        let media = match media {
            Some(m) => m.clone(),
            None => {
                let Some(cp) = attempt
                    .upload_session
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<InstagramUpload>(v.clone()).ok())
                else {
                    return Ok(Reconciliation::Unavailable {
                        reason: "no Instagram upload was recorded for this attempt".into(),
                    });
                };
                InstagramMedia {
                    id: cp.container_id,
                    account_id: cp.account_id,
                    caption_digest: cp.caption_digest,
                    uploaded_at: cp.created_at,
                }
            }
        };
        let status = self.container(media.account_id, &media.id).await?;
        Ok(match status.status_code.as_str() {
            "PUBLISHED" => match self.find_published(&media).await? {
                Some(item) => Reconciliation::Found {
                    post: self.post(media.account_id, item.id.clone(), Some(&item)),
                },
                None => Reconciliation::Unavailable {
                    reason: "Instagram says the reel was published, but it isn't among the \
                             account's recent posts with this caption; check the profile"
                        .into(),
                },
            },
            "FINISHED" | "IN_PROGRESS" | "ERROR" | "EXPIRED" => Reconciliation::NotFound,
            other => Reconciliation::Unavailable {
                reason: format!("Instagram reports the upload as {other:?}"),
            },
        })
    }

    async fn fetch_metrics(&self, post: &InstagramPost) -> Result<Vec<Metric>, AdapterError> {
        let item = self
            .media_item(post.account_id, &post.id, "like_count,comments_count")
            .await?;
        let collected_at = (self.now)();
        Ok([
            (
                item.like_count,
                "likes",
                "Instagram media like_count (hidden like counts may be reported as 0)",
            ),
            (
                item.comments_count,
                "comments",
                "Instagram media comments_count",
            ),
        ]
        .into_iter()
        .filter_map(|(value, name, definition)| {
            Some(Metric {
                name: name.to_owned(),
                value: value?,
                definition: definition.to_owned(),
                collected_at,
            })
        })
        .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instagram_timestamps_parse() {
        let t = parse_timestamp("2026-10-16T23:02:11+0000").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-16T23:02:11+00:00");
        assert!(parse_timestamp("2026-10-16T23:02:11Z").is_some());
        assert!(parse_timestamp("yesterday").is_none());
    }

    #[test]
    fn captions_digest_ignoring_outer_whitespace() {
        assert_eq!(caption_digest(" Hi\n\n#a "), caption_digest("Hi\n\n#a"));
        assert_ne!(caption_digest("Hi"), caption_digest("Hi!"));
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let s = IgSession {
            access_token: "IGAAsecret".into(),
            user_id: "17841400000000000".into(),
        };
        let shown = format!("{s:?}");
        assert!(!shown.contains("secret") && !shown.contains("1784"));
        let cp = InstagramUpload {
            account_id: AccountId::new(),
            container_id: "c1".into(),
            upload_url: "https://rupload.example/ig-api-upload/v24.0/c1".into(),
            total: 1,
            offset: 0,
            caption_digest: String::new(),
            created_at: Utc::now(),
        };
        assert!(!format!("{cp:?}").contains("rupload"));
    }

    #[test]
    fn page_token_errors_name_the_cause() {
        let e = |code, subcode| GraphError {
            message: "m".into(),
            code,
            error_subcode: subcode,
            ..Default::default()
        };
        for (subcode, words) in [
            (Some(460), "password"),
            (Some(463), "long-lived"),
            (Some(458), "removed"),
            (Some(492), "admin of the Facebook Page"),
            (None, "no longer accepts"),
        ] {
            let AdapterError::Authentication { message } =
                classify(StatusCode::BAD_REQUEST, &e(190, subcode))
            else {
                panic!("190 asks for a reconnect");
            };
            assert!(message.contains(words), "{message}");
        }
        for code in [10, 200, 299] {
            let AdapterError::Authentication { message } =
                classify(StatusCode::FORBIDDEN, &e(code, None))
            else {
                panic!("{code} asks for a reconnect");
            };
            assert!(message.contains("instagram_content_publish"), "{message}");
        }
    }

    #[test]
    fn page_debug_output_hides_the_token() {
        let p = FbPage {
            id: "1".into(),
            name: "Bakery".into(),
            access_token: "EAAGpage".into(),
            instagram_business_account: Some(LinkedInstagram {
                id: "1784".into(),
                username: "bakery".into(),
            }),
        };
        let shown = format!("{p:?}");
        assert!(shown.contains("bakery") && !shown.contains("EAAG") && !shown.contains("1784"));
    }

    #[test]
    fn errors_map_to_the_right_kind() {
        let e = |code, subcode, transient| GraphError {
            message: "m".into(),
            code,
            error_subcode: subcode,
            is_transient: transient,
            error_user_msg: None,
        };
        assert!(matches!(
            classify(StatusCode::BAD_REQUEST, &e(190, None, false)),
            AdapterError::Authentication { .. }
        ));
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, &e(10, None, false)),
            AdapterError::Authentication { .. }
        ));
        assert!(matches!(
            classify(StatusCode::BAD_REQUEST, &e(4, None, false)),
            AdapterError::RateLimited { .. }
        ));
        assert!(matches!(
            classify(StatusCode::BAD_REQUEST, &e(9, Some(2_207_042), false)),
            AdapterError::RateLimited { retry_after, .. } if retry_after == PUBLISH_LIMIT_PAUSE
        ));
        assert!(matches!(
            classify(StatusCode::INTERNAL_SERVER_ERROR, &e(1, None, false)),
            AdapterError::Transient { .. }
        ));
        assert!(matches!(
            classify(StatusCode::BAD_REQUEST, &e(100, None, true)),
            AdapterError::Transient { .. }
        ));
        assert!(matches!(
            classify(StatusCode::BAD_REQUEST, &e(100, Some(2_207_026), false)),
            AdapterError::Invalid { .. }
        ));
    }
}
