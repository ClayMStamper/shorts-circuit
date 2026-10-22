//! LinkedIn organization (Company Page) video posts through the versioned Marketing APIs
//! (`api.linkedin.com/rest`).
//!
//! - **Upload** is the Videos API: `initializeUpload` (owner = the organization, file size)
//!   returns a video URN and one pre-signed address per part; each `upload_step` sends one part
//!   and keeps its ETag; the last step calls `finalizeUpload` with the ETags. The checkpoint
//!   holds the parts and ETags so a restart resumes at the next part. A part address LinkedIn
//!   no longer accepts starts a fresh upload.
//! - **Processing** polls the video until it is `AVAILABLE`.
//! - **Publish** creates a post (Posts API) authored by the organization with the video as its
//!   media and the caption as "little text" commentary; the post URN comes back in the
//!   `x-restli-id` header.
//! - **Reconcile** lists the organization's recent posts and looks for one that uses the
//!   uploaded video.
//!
//! Every call sends `LinkedIn-Version` and `X-Restli-Protocol-Version: 2.0.0`. Tokens come from
//! a [`LinkedInAuth`] source and are never logged; part addresses aren't either.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveTime, Utc};
use reqwest::header::{CONTENT_TYPE, ETAG, RETRY_AFTER};
use reqwest::{RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, Platform, PublishAccess, PublishAttempt,
    ValidationIssue, linkedin,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::debug;

use crate::contract::{
    Adapter, AdapterError, Metric, Processing, PublishedPost, Reconciliation, Submission,
    UploadProgress,
};
use crate::youtube::Now;

pub const API_BASE: &str = "https://api.linkedin.com";
/// `LinkedIn-Version` (YYYYMM). LinkedIn supports each version for about a year;
/// `SHORTS_LINKEDIN_API_VERSION` overrides it.
pub const DEFAULT_API_VERSION: &str = "202609";
const RATE_LIMIT_MARGIN: Duration = Duration::from_secs(120);
/// Recent organization posts searched when reconciling.
const RECENT_POSTS: u32 = 20;

/// A connected organization's token and URN.
#[derive(Clone, PartialEq, Eq)]
pub struct LiSession {
    pub access_token: String,
    /// `urn:li:organization:<id>`.
    pub author: String,
}

impl fmt::Debug for LiSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiSession")
            .field("author", &self.author)
            .finish_non_exhaustive()
    }
}

/// Tokens for connected LinkedIn organizations.
#[async_trait]
pub trait LinkedInAuth: Send + Sync {
    /// [`AdapterError::Authentication`] means the account must be reconnected.
    async fn session(&self, account: AccountId) -> Result<LiSession, AdapterError>;
    /// LinkedIn refused `token`.
    async fn invalidate(&self, account: AccountId, token: &str);
}

/// A Company Page the token's member administers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Organization {
    /// `urn:li:organization:<id>`.
    pub urn: String,
    pub name: String,
}

/// One pre-signed part address.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadPart {
    pub url: String,
    pub first_byte: u64,
    pub last_byte: u64,
}

/// An initialized video upload.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedInUpload {
    pub account_id: AccountId,
    pub video: String,
    pub owner: String,
    /// The first part's address; the pipeline records a new upload when it changes.
    pub upload_url: String,
    pub parts: Vec<UploadPart>,
    #[serde(default)]
    pub upload_token: String,
    /// ETags of the parts sent so far, in order.
    pub etags: Vec<String>,
    pub total: u64,
    pub started_at: DateTime<Utc>,
}

impl fmt::Debug for LinkedInUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkedInUpload")
            .field("video", &self.video)
            .field("parts", &self.parts.len())
            .field("sent", &self.etags.len())
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

/// An uploaded video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedInMedia {
    /// The video URN (the pipeline records it as the remote media id).
    pub id: String,
    pub account_id: AccountId,
    pub owner: String,
    pub uploaded_at: DateTime<Utc>,
}

/// A published post.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedInPost {
    /// `urn:li:share:<id>` or `urn:li:ugcPost:<id>`.
    pub id: String,
    pub account_id: AccountId,
}

#[derive(Debug, Clone)]
pub struct LinkedInConfig {
    /// `https://api.linkedin.com`; tests point it at a fake.
    pub api_base: Url,
    pub api_version: String,
    pub poll_every: Duration,
    /// Per-part request timeout (parts are about 4 MB).
    pub part_timeout: Duration,
}

impl Default for LinkedInConfig {
    fn default() -> Self {
        Self {
            api_base: Url::parse(API_BASE).expect("valid URL"),
            api_version: DEFAULT_API_VERSION.to_owned(),
            poll_every: Duration::from_secs(10),
            part_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Initialized {
    value: InitializedValue,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializedValue {
    video: String,
    #[serde(default)]
    upload_token: String,
    #[serde(default)]
    upload_instructions: Vec<Instruction>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Instruction {
    upload_url: String,
    first_byte: u64,
    last_byte: u64,
}

#[derive(Debug, Default, Deserialize)]
struct VideoStatus {
    #[serde(default)]
    status: String,
}

#[derive(Debug, Default, Deserialize)]
struct Elements<T> {
    #[serde(default = "Vec::new")]
    elements: Vec<T>,
}

#[derive(Debug, Default, Deserialize)]
struct Post {
    #[serde(default)]
    id: String,
    #[serde(default)]
    content: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Acl {
    #[serde(default)]
    organization: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationBody {
    #[serde(default)]
    localized_name: String,
}

/// Percent-encodes a URN for use in a path or query (`:` → `%3A`, …).
pub fn encode_urn(urn: &str) -> String {
    let mut out = String::with_capacity(urn.len() * 2);
    for b in urn.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urn:li:organization:123` from `123`, a URN, or a Company Page admin URL containing the id.
pub fn organization_urn(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    if let Some(id) = s.strip_prefix("urn:li:organization:") {
        return (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            .then(|| format!("urn:li:organization:{id}"));
    }
    let tail = s
        .rsplit('/')
        .find(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))?;
    Some(format!("urn:li:organization:{tail}"))
}

fn transport(e: &reqwest::Error) -> AdapterError {
    if e.is_connect() {
        AdapterError::Transient {
            message: "could not connect to LinkedIn".to_owned(),
            retry_after: None,
        }
    } else {
        AdapterError::ResponseLost {
            message: if e.is_timeout() {
                "LinkedIn did not answer in time".to_owned()
            } else {
                "the connection to LinkedIn broke before the answer arrived".to_owned()
            },
        }
    }
}

pub struct LinkedInAdapter {
    http: reqwest::Client,
    config: LinkedInConfig,
    auth: Arc<dyn LinkedInAuth>,
    now: Now,
}

impl fmt::Debug for LinkedInAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkedInAdapter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl LinkedInAdapter {
    pub fn new(
        http: reqwest::Client,
        config: LinkedInConfig,
        auth: Arc<dyn LinkedInAuth>,
        now: Now,
    ) -> Self {
        Self {
            http,
            config,
            auth,
            now,
        }
    }

    fn rest(&self, path: &str) -> Url {
        let mut url = self.config.api_base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}/rest{path}"));
        url
    }

    fn versioned(&self, request: RequestBuilder, token: &str) -> RequestBuilder {
        request
            .bearer_auth(token)
            .header("LinkedIn-Version", self.config.api_version.as_str())
            .header("X-Restli-Protocol-Version", "2.0.0")
    }

    /// LinkedIn's daily limits reset at midnight UTC.
    fn until_reset(&self) -> Duration {
        let now = (self.now)();
        let next = (now.date_naive() + chrono::Duration::days(1))
            .and_time(NaiveTime::MIN)
            .and_utc();
        (next - now).to_std().unwrap_or(Duration::ZERO) + RATE_LIMIT_MARGIN
    }

    async fn error(&self, response: Response) -> AdapterError {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body: ErrorBody = response
            .text()
            .await
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let message = if body.message.trim().is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            body.message.clone()
        };
        debug!(status = status.as_u16(), code = ?body.code, "LinkedIn error");
        match status {
            StatusCode::UNAUTHORIZED => AdapterError::Authentication {
                message: format!("LinkedIn ended the sign-in ({message}); reconnect the page"),
            },
            StatusCode::FORBIDDEN => AdapterError::Authentication {
                message: format!(
                    "LinkedIn refused permission ({message}). The token needs \
                     w_organization_social (Community Management API) and you must be an admin \
                     of the page; reconnect with a new token"
                ),
            },
            StatusCode::TOO_MANY_REQUESTS => AdapterError::RateLimited {
                message: format!("LinkedIn's daily limit was reached: {message}"),
                retry_after: retry_after.unwrap_or_else(|| self.until_reset()),
            },
            StatusCode::UPGRADE_REQUIRED => AdapterError::Permanent {
                message: format!(
                    "LinkedIn no longer supports API version {} ({message}); set \
                     SHORTS_LINKEDIN_API_VERSION to a current one",
                    self.config.api_version
                ),
            },
            s if s.is_server_error() => AdapterError::Transient {
                message: format!("LinkedIn: {message}"),
                retry_after,
            },
            s if s.is_client_error() => AdapterError::Invalid {
                issues: vec![format!("LinkedIn rejected the post: {message}")],
            },
            _ => AdapterError::Permanent {
                message: format!("LinkedIn: {message}"),
            },
        }
    }

    /// Sends an authorized, versioned call, asking for a fresh token once if LinkedIn refuses
    /// it. Returns the successful response.
    async fn send(
        &self,
        account: AccountId,
        build: impl Fn(&LiSession) -> RequestBuilder,
    ) -> Result<Response, AdapterError> {
        let mut retried = false;
        loop {
            let session = self.auth.session(account).await?;
            let response = self
                .versioned(build(&session), &session.access_token)
                .send()
                .await
                .map_err(|e| transport(&e))?;
            if response.status().is_success() {
                return Ok(response);
            }
            if response.status() == StatusCode::UNAUTHORIZED && !retried {
                self.auth.invalidate(account, &session.access_token).await;
                retried = true;
                continue;
            }
            return Err(self.error(response).await);
        }
    }

    /// The Company Pages a pasted token's member administers. Needs `rw_organization_admin`
    /// (or `r_organization_admin`); without it LinkedIn answers 403 and the owner enters the
    /// page id instead.
    pub async fn organizations(&self, token: &str) -> Result<Vec<Organization>, AdapterError> {
        let response = self
            .versioned(self.http.get(self.rest("/organizationAcls")), token)
            .query(&[
                ("q", "roleAssignee"),
                ("role", "ADMINISTRATOR"),
                ("state", "APPROVED"),
            ])
            .send()
            .await
            .map_err(|e| transport(&e))?;
        if !response.status().is_success() {
            return Err(self.error(response).await);
        }
        let acls: Elements<Acl> = response.json().await.map_err(|e| transport(&e))?;
        let mut out = Vec::new();
        for acl in acls.elements {
            let Some(urn) = organization_urn(&acl.organization) else {
                continue;
            };
            if out.iter().any(|o: &Organization| o.urn == urn) {
                continue;
            }
            let name = self
                .organization_name(token, &urn)
                .await?
                .unwrap_or_else(|| default_name(&urn));
            out.push(Organization { urn, name });
        }
        Ok(out)
    }

    /// The page's name, or `None` when the token may not read it (it lacks the admin read
    /// scope, or the page doesn't exist). [`AdapterError::Authentication`] for a refused token.
    pub async fn organization_name(
        &self,
        token: &str,
        urn: &str,
    ) -> Result<Option<String>, AdapterError> {
        let id = urn.rsplit(':').next().unwrap_or_default();
        let response = self
            .versioned(
                self.http.get(self.rest(&format!("/organizations/{id}"))),
                token,
            )
            .send()
            .await
            .map_err(|e| transport(&e))?;
        match response.status() {
            s if s.is_success() => {
                let body: OrganizationBody = response.json().await.map_err(|e| transport(&e))?;
                Ok(Some(body.localized_name).filter(|n| !n.trim().is_empty()))
            }
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Ok(None),
            _ => Err(self.error(response).await),
        }
    }

    async fn initialize(&self, s: &Submission<'_>) -> Result<LinkedInUpload, AdapterError> {
        if s.size_bytes < linkedin::MIN_FILE_BYTES {
            return Err(AdapterError::Invalid {
                issues: vec![format!(
                    "LinkedIn needs a video file of at least {} KB",
                    linkedin::MIN_FILE_BYTES / 1024
                )],
            });
        }
        let account = s.account.id;
        let size = s.size_bytes;
        let mut url = self.rest("/videos");
        url.query_pairs_mut()
            .append_pair("action", "initializeUpload");
        let owner = self.auth.session(account).await?.author;
        let response = self
            .send(account, |_| {
                self.http.post(url.clone()).json(&json!({
                    "initializeUploadRequest": {
                        "owner": owner,
                        "fileSizeBytes": size,
                        "uploadCaptions": false,
                        "uploadThumbnail": false,
                    }
                }))
            })
            .await?;
        let init: Initialized = response.json().await.map_err(|e| transport(&e))?;
        let mut parts: Vec<UploadPart> = init
            .value
            .upload_instructions
            .into_iter()
            .map(|i| UploadPart {
                url: i.upload_url,
                first_byte: i.first_byte,
                last_byte: i.last_byte,
            })
            .collect();
        parts.sort_by_key(|p| p.first_byte);
        let covers = parts.first().is_some_and(|p| p.first_byte == 0)
            && parts.last().is_some_and(|p| p.last_byte + 1 == size)
            && parts
                .windows(2)
                .all(|w| w[0].last_byte + 1 == w[1].first_byte);
        if !covers {
            return Err(AdapterError::Transient {
                message: "LinkedIn's upload instructions don't cover the whole file".into(),
                retry_after: None,
            });
        }
        Ok(LinkedInUpload {
            account_id: account,
            video: init.value.video,
            owner,
            upload_url: parts[0].url.clone(),
            parts,
            upload_token: init.value.upload_token,
            etags: Vec::new(),
            total: size,
            started_at: (self.now)(),
        })
    }

    async fn read_part(
        &self,
        s: &Submission<'_>,
        part: &UploadPart,
    ) -> Result<Vec<u8>, AdapterError> {
        let io = |e: std::io::Error| AdapterError::Permanent {
            message: format!("could not read the rendered video: {e}"),
        };
        let mut file = tokio::fs::File::open(s.media_path).await.map_err(io)?;
        file.seek(std::io::SeekFrom::Start(part.first_byte))
            .await
            .map_err(io)?;
        let len = part.last_byte - part.first_byte + 1;
        let mut buf = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX)];
        file.read_exact(&mut buf).await.map_err(io)?;
        Ok(buf)
    }

    async fn finalize(&self, cp: &LinkedInUpload) -> Result<(), AdapterError> {
        let mut url = self.rest("/videos");
        url.query_pairs_mut()
            .append_pair("action", "finalizeUpload");
        self.send(cp.account_id, |_| {
            self.http.post(url.clone()).json(&json!({
                "finalizeUploadRequest": {
                    "video": cp.video,
                    "uploadToken": cp.upload_token,
                    "uploadedPartIds": cp.etags,
                }
            }))
        })
        .await?;
        Ok(())
    }

    fn post(&self, account: AccountId, urn: String) -> PublishedPost<LinkedInPost> {
        PublishedPost {
            url: Some(linkedin::post_url(&urn)),
            remote_id: urn.clone(),
            note: None,
            post: LinkedInPost {
                id: urn,
                account_id: account,
            },
        }
    }

    /// The organization's recent post that uses `video`, if any. `Ok(None)` means LinkedIn
    /// listed the posts and none uses it.
    async fn find_post(&self, media: &LinkedInMedia) -> Result<Option<String>, AdapterError> {
        let count = RECENT_POSTS.to_string();
        let response = self
            .send(media.account_id, |_| {
                self.http
                    .get(self.rest("/posts"))
                    .header("X-RestLi-Method", "FINDER")
                    .query(&[
                        ("author", media.owner.as_str()),
                        ("q", "author"),
                        ("count", count.as_str()),
                        ("sortBy", "LAST_MODIFIED"),
                    ])
            })
            .await?;
        let posts: Elements<Post> = response.json().await.map_err(|e| transport(&e))?;
        Ok(posts
            .elements
            .into_iter()
            .find(|p| {
                p.content
                    .as_ref()
                    .and_then(|c| c.pointer("/media/id"))
                    .and_then(Value::as_str)
                    == Some(media.id.as_str())
            })
            .map(|p| p.id))
    }
}

/// `LinkedIn page <id>`, for a page whose name can't be read.
pub fn default_name(urn: &str) -> String {
    format!(
        "LinkedIn page {}",
        urn.rsplit(':').next().unwrap_or_default()
    )
}

#[async_trait]
impl Adapter for LinkedInAdapter {
    type UploadCheckpoint = LinkedInUpload;
    type RemoteMedia = LinkedInMedia;
    type RemotePost = LinkedInPost;

    fn platform(&self) -> Platform {
        Platform::Linkedin
    }

    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError> {
        let now = (self.now)();
        if let Err(AdapterError::Authentication { message }) = self.auth.session(account.id).await {
            return Ok(Capabilities::reconnect_required(
                &format!("Reconnect the page to post automatically ({message})."),
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
                format!("Posts publicly as {}.", account.display_name),
                "LinkedIn tokens last 60 days. The app warns a week before; reconnect with a \
                 new token from the LinkedIn developer portal's token generator."
                    .to_owned(),
                "LinkedIn decides where the post appears in people's feeds.".to_owned(),
            ],
            checked_at: now,
        })
    }

    async fn validate(&self, s: &Submission<'_>) -> Result<Vec<ValidationIssue>, AdapterError> {
        let content = s.publication.content();
        let mut issues = Platform::Linkedin.requirements().check(
            s.media,
            s.size_bytes,
            &content.caption,
            &content.hashtags,
        );
        let extension = shorts_domain::youtube::extension(&s.media_path.to_string_lossy());
        if !linkedin::FILE_EXTENSIONS.contains(&extension.as_str()) {
            issues.push(ValidationIssue::UnsupportedFormat { extension });
        }
        // Escaping and hashtag templates make the posted text longer than the caption.
        let commentary = linkedin::commentary(&content.caption, &content.hashtags);
        let chars = commentary.chars().count();
        if chars > linkedin::COMMENTARY_MAX_CHARS
            && !issues
                .iter()
                .any(|i| matches!(i, ValidationIssue::CaptionTooLong { .. }))
        {
            issues.push(ValidationIssue::CaptionTooLong {
                chars,
                max: linkedin::COMMENTARY_MAX_CHARS,
            });
        }
        Ok(issues)
    }

    async fn begin_upload(&self, s: &Submission<'_>) -> Result<LinkedInUpload, AdapterError> {
        self.initialize(s).await
    }

    async fn upload_step(
        &self,
        s: &Submission<'_>,
        mut cp: LinkedInUpload,
    ) -> Result<UploadProgress<LinkedInUpload, LinkedInMedia>, AdapterError> {
        if cp.total != s.size_bytes {
            return Err(AdapterError::Permanent {
                message: "the rendered video changed during the upload; publish it again".into(),
            });
        }
        if let Some(part) = cp.parts.get(cp.etags.len()).cloned() {
            let bytes = self.read_part(s, &part).await?;
            let sent = self
                .http
                .put(part.url.as_str())
                .header(CONTENT_TYPE, "application/octet-stream")
                .timeout(self.config.part_timeout)
                .body(bytes)
                .send()
                .await
                .map_err(|e| match transport(&e) {
                    // Re-sending a part is harmless.
                    AdapterError::ResponseLost { message } => AdapterError::Transient {
                        message,
                        retry_after: None,
                    },
                    other => other,
                })?;
            let status = sent.status();
            if status.is_success() {
                let etag = sent
                    .headers()
                    .get(ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
                    .ok_or_else(|| AdapterError::Transient {
                        message: "LinkedIn did not confirm the uploaded part".into(),
                        retry_after: None,
                    })?;
                cp.etags.push(etag);
            } else if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                return Err(AdapterError::Transient {
                    message: format!("LinkedIn could not take a part (HTTP {})", status.as_u16()),
                    retry_after: None,
                });
            } else {
                // The part address expired or was refused: start a new upload.
                debug!(
                    status = status.as_u16(),
                    "LinkedIn part refused; starting over"
                );
                return Ok(UploadProgress::InProgress {
                    checkpoint: self.initialize(s).await?,
                    fraction: 0.0,
                });
            }
            if cp.etags.len() < cp.parts.len() {
                let sent_bytes = cp.parts[cp.etags.len() - 1].last_byte + 1;
                #[allow(clippy::cast_precision_loss)]
                let fraction = sent_bytes as f64 / cp.total as f64;
                return Ok(UploadProgress::InProgress {
                    checkpoint: cp,
                    fraction,
                });
            }
        }
        self.finalize(&cp).await?;
        Ok(UploadProgress::Complete {
            media: LinkedInMedia {
                id: cp.video,
                account_id: cp.account_id,
                owner: cp.owner,
                uploaded_at: (self.now)(),
            },
        })
    }

    async fn check_processing(&self, media: &LinkedInMedia) -> Result<Processing, AdapterError> {
        let path = format!("/videos/{}", encode_urn(&media.id));
        let response = self
            .send(media.account_id, |_| self.http.get(self.rest(&path)))
            .await?;
        let video: VideoStatus = response.json().await.map_err(|e| transport(&e))?;
        Ok(match video.status.as_str() {
            "AVAILABLE" => Processing::Ready,
            "PROCESSING_FAILED" => Processing::Failed {
                message: "LinkedIn could not process the video".into(),
            },
            _ => Processing::Pending {
                poll_after: self.config.poll_every,
                detail: Some("LinkedIn is processing the video".into()),
            },
        })
    }

    async fn publish(
        &self,
        s: &Submission<'_>,
        media: &LinkedInMedia,
    ) -> Result<PublishedPost<LinkedInPost>, AdapterError> {
        let content = s.publication.content();
        let body = json!({
            "author": media.owner,
            "commentary": linkedin::commentary(&content.caption, &content.hashtags),
            "visibility": "PUBLIC",
            "distribution": {
                "feedDistribution": "MAIN_FEED",
                "targetEntities": [],
                "thirdPartyDistributionChannels": [],
            },
            "content": {
                "media": {
                    "title": linkedin::title(&content.caption),
                    "id": media.id,
                }
            },
            "lifecycleState": "PUBLISHED",
            "isReshareDisabledByAuthor": false,
        });
        let response = self
            .send(media.account_id, |_| {
                self.http.post(self.rest("/posts")).json(&body)
            })
            .await?;
        let urn = response
            .headers()
            .get("x-restli-id")
            .or_else(|| response.headers().get("x-linkedin-id"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .filter(|u| u.starts_with("urn:li:"));
        match urn {
            Some(urn) => Ok(self.post(media.account_id, urn)),
            // Accepted, but the id is missing: the pipeline reconciles by video.
            None => Err(AdapterError::ResponseLost {
                message: "LinkedIn accepted the post without saying its id".into(),
            }),
        }
    }

    async fn reconcile(
        &self,
        attempt: &PublishAttempt,
        media: Option<&LinkedInMedia>,
    ) -> Result<Reconciliation<LinkedInPost>, AdapterError> {
        let media = match media {
            Some(m) => m.clone(),
            None => {
                let Some(cp) = attempt
                    .upload_session
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<LinkedInUpload>(v.clone()).ok())
                else {
                    return Ok(Reconciliation::Unavailable {
                        reason: "no LinkedIn upload was recorded for this attempt".into(),
                    });
                };
                LinkedInMedia {
                    id: cp.video,
                    account_id: cp.account_id,
                    owner: cp.owner,
                    uploaded_at: cp.started_at,
                }
            }
        };
        match self.find_post(&media).await {
            Ok(Some(urn)) => Ok(Reconciliation::Found {
                post: self.post(media.account_id, urn),
            }),
            Ok(None) => Ok(Reconciliation::NotFound),
            Err(AdapterError::Authentication { message }) => Ok(Reconciliation::Unavailable {
                reason: format!(
                    "the page's posts can't be read to check ({message}); add \
                     r_organization_social to the token, or check the page yourself"
                ),
            }),
            Err(e) => Err(e),
        }
    }

    async fn fetch_metrics(&self, _post: &LinkedInPost) -> Result<Vec<Metric>, AdapterError> {
        // Organization share statistics need a separate API and scope; not collected yet.
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urns_are_percent_encoded() {
        assert_eq!(
            encode_urn("urn:li:video:C5F10AQ"),
            "urn%3Ali%3Avideo%3AC5F10AQ"
        );
    }

    #[test]
    fn organization_ids_are_read_from_several_forms() {
        let urn = Some("urn:li:organization:123".to_owned());
        assert_eq!(organization_urn("123"), urn);
        assert_eq!(organization_urn(" urn:li:organization:123 "), urn);
        assert_eq!(
            organization_urn("https://www.linkedin.com/company/123/admin/"),
            urn
        );
        assert_eq!(
            organization_urn("https://www.linkedin.com/company/123/"),
            urn
        );
        assert_eq!(organization_urn("urn:li:organization:x"), None);
        assert_eq!(organization_urn("timeless-way"), None);
    }

    #[test]
    fn debug_output_hides_tokens_and_part_addresses() {
        let s = LiSession {
            access_token: "AQVsecret".into(),
            author: "urn:li:organization:1".into(),
        };
        assert!(!format!("{s:?}").contains("secret"));
        let cp = LinkedInUpload {
            account_id: AccountId::new(),
            video: "urn:li:video:V".into(),
            owner: "urn:li:organization:1".into(),
            upload_url: "https://www.linkedin.com/dms-uploads/secret".into(),
            parts: vec![UploadPart {
                url: "https://www.linkedin.com/dms-uploads/secret".into(),
                first_byte: 0,
                last_byte: 1,
            }],
            upload_token: String::new(),
            etags: Vec::new(),
            total: 2,
            started_at: Utc::now(),
        };
        assert!(!format!("{cp:?}").contains("secret"));
    }
}
