//! Instagram Reels through the Instagram API with Instagram Login (`graph.instagram.com`).
//!
//! - **Upload** creates a Reels media container with `upload_type=resumable` (the caption goes
//!   in with it), then sends the rendered file to the container's `rupload` address in one
//!   request. Re-sending to the same container replaces what was there, so an interrupted
//!   upload simply starts again; nothing has to be hosted at a public URL. Containers expire
//!   after 24 hours, so an older checkpoint opens a fresh container.
//! - **Processing** polls the container's `status_code` until it is `FINISHED`.
//! - **Publish** checks the account's 24-hour publishing limit, then calls `media_publish` and
//!   reads the new reel's permalink.
//! - **Reconcile** (after a lost publish answer) reads the container: `PUBLISHED` means the reel
//!   exists, and it is found among the account's recent media by its caption; `FINISHED` means
//!   nothing was published.
//!
//! Tokens come from an [`InstagramAuth`] source (the desktop keeps a long-lived token per
//! account and renews it). They go in `Authorization` headers and are never logged; the
//! account's Instagram user id is kept out of logs too.

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

pub const GRAPH_BASE: &str = "https://graph.instagram.com";
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

const CONNECT_NOTE: &str = "The sign-in lasts 60 days. The app renews it automatically while it \
    runs; if it lapses, generate a new token in the Meta app dashboard and reconnect.";

/// A connected account's long-lived token and Instagram user id.
#[derive(Clone, PartialEq, Eq)]
pub struct IgSession {
    pub access_token: String,
    /// The professional account's Instagram user id (`user_id` from `/me`).
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

/// The signed-in professional account, from `GET /me`.
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

/// A renewed long-lived token.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct RefreshedToken {
    pub access_token: String,
    /// Seconds.
    #[serde(default)]
    pub expires_in: i64,
}

impl fmt::Debug for RefreshedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshedToken")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
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
    /// SHA-256 of the caption, to recognise the reel among recent media.
    pub caption_digest: String,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for InstagramUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstagramUpload")
            .field("container_id", &self.container_id)
            .field("total", &self.total)
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
    /// `https://graph.instagram.com`; tests point it at a fake.
    pub graph_base: Url,
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
}

#[derive(Debug, Default, Deserialize)]
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
    fn is_auth(&self, status: StatusCode) -> bool {
        status == StatusCode::UNAUTHORIZED || matches!(self.code, 102 | 190 | 463 | 467)
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
    let body: ErrorBody = response
        .text()
        .await
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    (status, body.error)
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
            message: format!("Instagram ended the sign-in ({text}); reconnect the account"),
        };
    }
    match (e.code, e.error_subcode) {
        (10 | 200..=299, _) => AdapterError::Authentication {
            message: format!(
                "Instagram refused permission ({text}); reconnect the account and allow \
                 publishing content"
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
        let mut url = self.config.graph_base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}/{}{path}", self.config.api_version));
        url
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

    /// Who a freshly pasted token belongs to. Used when connecting.
    pub async fn profile(&self, access_token: &str) -> Result<IgProfile, AdapterError> {
        let response = self
            .http
            .get(self.graph("/me"))
            .query(&[("fields", "user_id,username,account_type")])
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| transport(&e))?;
        if !response.status().is_success() {
            let (status, error) = read_error(response).await;
            return Err(classify(status, &error));
        }
        response.json().await.map_err(|e| transport(&e))
    }

    /// Renews a long-lived token (it must be at least 24 hours old) for another 60 days.
    pub async fn refresh_token(&self, access_token: &str) -> Result<RefreshedToken, AdapterError> {
        let mut url = self.config.graph_base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}/refresh_access_token"));
        // Instagram takes this token only as a query parameter; the URL is never logged.
        let response = self
            .http
            .get(url)
            .query(&[
                ("grant_type", "ig_refresh_token"),
                ("access_token", access_token),
            ])
            .send()
            .await
            .map_err(|e| transport(&e))?;
        if !response.status().is_success() {
            let (status, error) = read_error(response).await;
            return Err(classify(status, &error));
        }
        response.json().await.map_err(|e| transport(&e))
    }

    async fn create_container(&self, s: &Submission<'_>) -> Result<InstagramUpload, AdapterError> {
        if s.size_bytes == 0 {
            return Err(AdapterError::Invalid {
                issues: vec!["the rendered file is empty".to_owned()],
            });
        }
        let content = s.publication.content();
        let caption = full_caption(&content.caption, &content.hashtags);
        let created: Created = self
            .call(s.account.id, |session| {
                self.http
                    .post(self.graph(&format!("/{}/media", session.user_id)))
                    .bearer_auth(&session.access_token)
                    .form(&[
                        ("media_type", "REELS"),
                        ("upload_type", "resumable"),
                        ("caption", caption.as_str()),
                        ("share_to_feed", "true"),
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
        let session = self.auth.session(cp.account_id).await?;
        let sent = self
            .http
            .post(cp.upload_url.as_str())
            .header(AUTHORIZATION, format!("OAuth {}", session.access_token))
            .header("offset", "0")
            .header("file_size", cp.total.to_string())
            .timeout(self.upload_timeout(cp.total))
            .body(bytes)
            .send()
            .await;
        let response = match sent {
            Ok(r) => r,
            // Sending to the same container again replaces the bytes, so a broken upload is
            // simply repeated.
            Err(e) => {
                return Err(AdapterError::Transient {
                    message: match transport(&e) {
                        AdapterError::Transient { message, .. }
                        | AdapterError::ResponseLost { message } => {
                            format!("{message}; the upload starts again")
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
                        "Instagram did not accept the upload ({}); it starts again",
                        answer.message.unwrap_or_else(|| "no reason given".into())
                    ),
                    retry_after: None,
                });
            }
            return Ok(UploadProgress::Complete {
                media: InstagramMedia {
                    id: cp.container_id,
                    account_id: cp.account_id,
                    caption_digest: cp.caption_digest,
                    uploaded_at: (self.now)(),
                },
            });
        }
        let (status, error) = read_error(response).await;
        if error.is_auth(status) {
            self.auth
                .invalidate(cp.account_id, &session.access_token)
                .await;
            return Err(AdapterError::Transient {
                message: "Instagram asked for a fresh sign-in token".into(),
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
            caption_digest: String::new(),
            created_at: Utc::now(),
        };
        assert!(!format!("{cp:?}").contains("rupload"));
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
