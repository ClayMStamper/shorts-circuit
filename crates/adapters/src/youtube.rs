//! YouTube Shorts through the YouTube Data API v3.
//!
//! - **Upload** is a resumable `videos.insert`: [`YouTubeAdapter::begin_upload`] opens a session
//!   (title, description, tags and privacy go in with it), and each `upload_step` sends one
//!   chunk. The checkpoint holds the session URL and byte offset, so a restart resumes
//!   mid-file: the first step of a process (or after any failed chunk) asks the session how
//!   many bytes it has before sending more. If the session has expired, the step looks for the
//!   video by the marker tag before starting over, so a lost final response never becomes a
//!   second upload.
//! - **Processing** polls `videos.list` (`status`, `processingDetails`).
//! - **Publish** confirms the video and its visibility. The privacy status is set at upload
//!   time (the narrow `youtube.upload` scope can't change it later): private while the account
//!   is marked private-only (an unaudited Google Cloud project), otherwise public. If YouTube
//!   kept a video private anyway, the published post says so.
//! - **Quota** is counted per Pacific-time day in a [`QuotaLedger`]. A call that would go over
//!   the configured daily limits, or a `quotaExceeded` answer, becomes a rate-limit pause until
//!   the quota resets; `rateLimitExceeded` pauses briefly.
//!
//! Access tokens come from an [`AccessTokens`] source (the desktop refreshes them with the
//! owner's OAuth client); they are sent as bearer headers and never logged. Session URLs are
//! not logged either.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::America::Los_Angeles;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE, RETRY_AFTER};
use reqwest::{RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shorts_domain::platform::full_caption;
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, Platform, PublicationId, PublishAccess,
    PublishAttempt, ValidationIssue, youtube,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, warn};

use crate::contract::{
    Adapter, AdapterError, Metric, Processing, PublishedPost, Reconciliation, Submission,
    UploadProgress,
};

pub const API_BASE: &str = "https://www.googleapis.com";
/// Quota bucket of `videos.insert` calls.
pub const BUCKET_UPLOADS: &str = "youtube:uploads";
/// Quota bucket of every other call.
pub const BUCKET_UNITS: &str = "youtube:units";
/// Units an upload needs left in [`BUCKET_UNITS`] to be worth starting: enough to watch it
/// process and confirm it.
const UNITS_PER_UPLOAD: u32 = 30;
/// A multiple of 256 KiB, as resumable uploads require.
pub const DEFAULT_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(60);
/// After a reset, wait a little in case the clocks disagree.
const RESET_MARGIN: Duration = Duration::from_secs(120);
/// Recent uploads searched for a marker.
const MARKER_SEARCH_DEPTH: u32 = 25;
const TAGS_MAX_CHARS: usize = 450;
/// A just-finished upload may take a moment to show up in `videos.list`.
const LISTING_GRACE: chrono::Duration = chrono::Duration::minutes(10);

pub const PRIVATE_ONLY_NOTE: &str = "Uploads go up as private. Google has restricted uploads \
    from API projects that haven't passed YouTube's compliance audit to private since 2020; \
    when your project has passed (or YouTube publishes your uploads publicly), switch this \
    channel to public uploads in Settings.";
const KEPT_PRIVATE_NOTE: &str = "YouTube kept this video private, most likely because the Google \
    Cloud project hasn't passed YouTube's API compliance audit. It is on your channel; check its \
    visibility in YouTube Studio.";
const UPLOADED_PRIVATE_NOTE: &str = "Uploaded as private (this channel is set to private \
    uploads until your Google Cloud project passes YouTube's API audit). Open it in YouTube \
    Studio to check or change its visibility.";

/// Bearer tokens for connected accounts.
#[async_trait]
pub trait AccessTokens: Send + Sync {
    /// A currently valid access token. [`AdapterError::Authentication`] means the account
    /// must be reconnected.
    async fn access_token(&self, account: AccountId) -> Result<String, AdapterError>;
    /// The API refused `token`; drop it so the next call gets a fresh one.
    async fn invalidate(&self, account: AccountId, token: &str);
}

/// Where quota spend is recorded (the desktop keeps it in SQLite).
#[async_trait]
pub trait QuotaLedger: Send + Sync {
    async fn used(&self, bucket: &str, day: NaiveDate) -> Result<u32, String>;
    async fn spend(&self, bucket: &str, day: NaiveDate, units: u32) -> Result<u32, String>;
    /// The provider said the quota is gone.
    async fn exhaust(&self, bucket: &str, day: NaiveDate, limit: u32) -> Result<(), String>;
}

/// In-memory [`QuotaLedger`] for tests.
#[derive(Debug, Default)]
pub struct MemoryQuota {
    spent: Mutex<HashMap<(String, NaiveDate), u32>>,
}

#[async_trait]
impl QuotaLedger for MemoryQuota {
    async fn used(&self, bucket: &str, day: NaiveDate) -> Result<u32, String> {
        let spent = self.spent.lock().expect("quota lock");
        Ok(spent.get(&(bucket.to_owned(), day)).copied().unwrap_or(0))
    }

    async fn spend(&self, bucket: &str, day: NaiveDate, units: u32) -> Result<u32, String> {
        let mut spent = self.spent.lock().expect("quota lock");
        let total = spent.entry((bucket.to_owned(), day)).or_insert(0);
        *total += units;
        Ok(*total)
    }

    async fn exhaust(&self, bucket: &str, day: NaiveDate, limit: u32) -> Result<(), String> {
        let mut spent = self.spent.lock().expect("quota lock");
        let total = spent.entry((bucket.to_owned(), day)).or_insert(0);
        *total = (*total).max(limit);
        Ok(())
    }
}

/// The project's daily quotas. Google's defaults unless the owner got an increase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLimits {
    pub uploads_per_day: u32,
    pub units_per_day: u32,
}

impl Default for QuotaLimits {
    fn default() -> Self {
        Self {
            uploads_per_day: youtube::DEFAULT_UPLOADS_PER_DAY,
            units_per_day: youtube::DEFAULT_UNITS_PER_DAY,
        }
    }
}

/// YouTube's quota day: the calendar date in Pacific time.
pub fn quota_day(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&Los_Angeles).date_naive()
}

/// When the quota day containing `at` ends (midnight Pacific time).
pub fn quota_resets_at(at: DateTime<Utc>) -> DateTime<Utc> {
    let next = quota_day(at).succ_opt().unwrap_or(NaiveDate::MAX);
    Los_Angeles
        .from_local_datetime(&next.and_time(NaiveTime::MIN))
        .earliest()
        .map_or(at + chrono::Duration::days(1), |t| t.with_timezone(&Utc))
}

/// Today's spend against the limits, for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaReport {
    pub day: NaiveDate,
    pub uploads_used: u32,
    pub uploads_limit: u32,
    pub units_used: u32,
    pub units_limit: u32,
    pub resets_at: DateTime<Utc>,
    /// Set when less than a fifth of either quota is left.
    pub warning: Option<String>,
}

pub async fn quota_report(
    ledger: &dyn QuotaLedger,
    limits: QuotaLimits,
    now: DateTime<Utc>,
) -> Result<QuotaReport, String> {
    let day = quota_day(now);
    let uploads_used = ledger.used(BUCKET_UPLOADS, day).await?;
    let units_used = ledger.used(BUCKET_UNITS, day).await?;
    let low = |used: u32, limit: u32| used.saturating_mul(5) >= limit.saturating_mul(4);
    let warning = if uploads_used >= limits.uploads_per_day
        || units_used.saturating_add(UNITS_PER_UPLOAD) > limits.units_per_day
    {
        Some(
            "Today's YouTube API quota is used up; uploads wait until it resets at midnight \
             Pacific time."
                .to_owned(),
        )
    } else if low(uploads_used, limits.uploads_per_day) || low(units_used, limits.units_per_day) {
        Some(format!(
            "Today's YouTube API quota is running low ({uploads_used} of {} uploads, \
             {units_used} of {} units); later uploads may wait until midnight Pacific time.",
            limits.uploads_per_day, limits.units_per_day
        ))
    } else {
        None
    };
    Ok(QuotaReport {
        day,
        uploads_used,
        uploads_limit: limits.uploads_per_day,
        units_used,
        units_limit: limits.units_per_day,
        resets_at: quota_resets_at(now),
        warning,
    })
}

/// The tag that identifies this app's upload of a publication revision, for finding the video
/// when a response was lost. Derived from internal ids only.
pub fn marker(publication: PublicationId, revision: u32) -> String {
    let digest = Sha256::digest(format!("{publication}:{revision}").as_bytes());
    format!("sc{}", &hex_lower(&digest)[..14])
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An open resumable upload session.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YouTubeUpload {
    pub account_id: AccountId,
    /// The session URI from `videos.insert`; it authorizes uploading to this session only.
    pub upload_url: String,
    /// Bytes YouTube has confirmed.
    pub offset: u64,
    pub total: u64,
    pub marker: String,
    pub privacy: String,
    pub started_at: DateTime<Utc>,
}

impl fmt::Debug for YouTubeUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YouTubeUpload")
            .field("upload_url", &"<redacted>")
            .field("offset", &self.offset)
            .field("total", &self.total)
            .field("marker", &self.marker)
            .finish_non_exhaustive()
    }
}

/// An uploaded video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YouTubeMedia {
    /// The video id (the pipeline records it as the remote media id).
    pub id: String,
    pub account_id: AccountId,
    /// The privacy status the upload asked for.
    pub privacy: String,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YouTubePost {
    pub id: String,
    pub account_id: AccountId,
    /// As YouTube reported it when publishing was confirmed.
    pub privacy_status: String,
}

#[derive(Debug, Clone)]
pub struct YouTubeConfig {
    /// `https://www.googleapis.com`; tests point it at a fake.
    pub api_base: Url,
    pub chunk_bytes: u64,
    pub limits: QuotaLimits,
    pub poll_every: Duration,
}

impl Default for YouTubeConfig {
    fn default() -> Self {
        Self {
            api_base: Url::parse(API_BASE).expect("valid URL"),
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            limits: QuotaLimits::default(),
            poll_every: Duration::from_secs(15),
        }
    }
}

pub type Now = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct YouTubeAdapter {
    http: reqwest::Client,
    config: YouTubeConfig,
    tokens: Arc<dyn AccessTokens>,
    quota: Arc<dyn QuotaLedger>,
    now: Now,
    /// Sessions whose offset this process last heard from YouTube itself. Anything else (a
    /// restart, a failed chunk) asks the session first.
    confirmed: Mutex<HashMap<String, u64>>,
}

impl fmt::Debug for YouTubeAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YouTubeAdapter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: ErrorInner,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorInner {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<ErrorItem>,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorItem {
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Video {
    id: String,
    #[serde(default)]
    snippet: Option<VideoSnippet>,
    #[serde(default)]
    status: Option<VideoStatus>,
    #[serde(default)]
    processing_details: Option<ProcessingDetails>,
    #[serde(default)]
    statistics: Option<HashMap<String, String>>,
}

#[derive(Debug, Default, Deserialize)]
struct VideoSnippet {
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoStatus {
    #[serde(default)]
    upload_status: String,
    #[serde(default)]
    privacy_status: String,
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default)]
    rejection_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessingDetails {
    #[serde(default)]
    processing_status: String,
    #[serde(default)]
    processing_failure_reason: Option<String>,
    #[serde(default)]
    processing_progress: Option<ProcessingProgress>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessingProgress {
    #[serde(default)]
    parts_total: Option<String>,
    #[serde(default)]
    parts_processed: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Listing<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Channel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    snippet: Option<ChannelSnippet>,
    #[serde(default)]
    content_details: Option<ChannelContent>,
}

#[derive(Debug, Deserialize)]
struct ChannelSnippet {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelContent {
    related_playlists: RelatedPlaylists,
}

#[derive(Debug, Deserialize)]
struct RelatedPlaylists {
    #[serde(default)]
    uploads: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistItem {
    content_details: PlaylistItemContent,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistItemContent {
    video_id: String,
}

/// The signed-in channel, from `channels.list?mine=true`.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    /// The channel id (kept in the credential store, never in logs).
    pub id: String,
    pub title: String,
}

impl fmt::Debug for ChannelInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelInfo")
            .field("id", &"<redacted>")
            .field("title", &self.title)
            .finish()
    }
}

enum SessionStatus {
    /// The upload finished; the video id.
    Complete(String),
    Incomplete(u64),
    Expired,
}

fn transport(e: &reqwest::Error) -> AdapterError {
    if e.is_connect() {
        AdapterError::Transient {
            message: "could not connect to YouTube".to_owned(),
            retry_after: None,
        }
    } else {
        // Sent (or partly sent) without an answer: it may have taken effect.
        AdapterError::ResponseLost {
            message: if e.is_timeout() {
                "YouTube did not answer in time".to_owned()
            } else {
                "the connection to YouTube broke before the answer arrived".to_owned()
            },
        }
    }
}

fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (at - now).to_std().unwrap_or(Duration::ZERO)
}

/// `Range: bytes=0-N` → `N + 1` bytes received.
fn received(response: &Response) -> u64 {
    response
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('-').next())
        .and_then(|n| n.trim().parse::<u64>().ok())
        .map_or(0, |last| last + 1)
}

impl YouTubeAdapter {
    pub fn new(
        http: reqwest::Client,
        config: YouTubeConfig,
        tokens: Arc<dyn AccessTokens>,
        quota: Arc<dyn QuotaLedger>,
        now: Now,
    ) -> Self {
        Self {
            http,
            config,
            tokens,
            quota,
            now,
            confirmed: Mutex::new(HashMap::new()),
        }
    }

    pub fn limits(&self) -> QuotaLimits {
        self.config.limits
    }

    pub fn quota_ledger(&self) -> Arc<dyn QuotaLedger> {
        self.quota.clone()
    }

    fn api(&self, path: &str) -> Url {
        let mut url = self.config.api_base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}{path}"));
        url
    }

    fn limit(&self, bucket: &str) -> u32 {
        if bucket == BUCKET_UPLOADS {
            self.config.limits.uploads_per_day
        } else {
            self.config.limits.units_per_day
        }
    }

    fn quota_paused(&self, message: String) -> AdapterError {
        let now = (self.now)();
        AdapterError::RateLimited {
            message,
            retry_after: until(quota_resets_at(now), now) + RESET_MARGIN,
        }
    }

    /// Refuses a call that would go over today's quota; otherwise records its cost.
    async fn reserve(&self, bucket: &str, cost: u32, headroom: u32) -> Result<(), AdapterError> {
        let day = quota_day((self.now)());
        let ledger = |e: String| AdapterError::Transient {
            message: format!("quota ledger: {e}"),
            retry_after: None,
        };
        let used = self.quota.used(bucket, day).await.map_err(ledger)?;
        if used.saturating_add(cost).saturating_add(headroom) > self.limit(bucket) {
            return Err(self.quota_paused(format!(
                "today's YouTube API quota is used up ({used} of {} {}); continuing after \
                 midnight Pacific time",
                self.limit(bucket),
                if bucket == BUCKET_UPLOADS {
                    "uploads"
                } else {
                    "units"
                }
            )));
        }
        self.quota.spend(bucket, day, cost).await.map_err(ledger)?;
        Ok(())
    }

    /// Sends an authorized request, refreshing the token once if YouTube refuses it.
    async fn send(
        &self,
        account: AccountId,
        build: impl Fn(&str) -> RequestBuilder,
    ) -> Result<Response, AdapterError> {
        let token = self.tokens.access_token(account).await?;
        let response = build(&token).send().await.map_err(|e| transport(&e))?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        self.tokens.invalidate(account, &token).await;
        let token = self.tokens.access_token(account).await?;
        let response = build(&token).send().await.map_err(|e| transport(&e))?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.tokens.invalidate(account, &token).await;
            return Err(AdapterError::Authentication {
                message: "YouTube refused the sign-in; reconnect the channel".to_owned(),
            });
        }
        Ok(response)
    }

    /// Turns a non-success answer into an error, counting the day as used up when YouTube
    /// says the quota is gone.
    async fn error(&self, response: Response, bucket: &str) -> AdapterError {
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
        let reason = body
            .error
            .errors
            .first()
            .map(|e| e.reason.clone())
            .unwrap_or_default();
        let message = if body.error.message.is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            body.error.message.clone()
        };
        debug!(status = status.as_u16(), %reason, "YouTube error");
        match reason.as_str() {
            "quotaExceeded" | "dailyLimitExceeded" => {
                let day = quota_day((self.now)());
                if let Err(e) = self.quota.exhaust(bucket, day, self.limit(bucket)).await {
                    warn!(error = %e, "could not record exhausted quota");
                }
                self.quota_paused(
                    "YouTube says today's API quota is used up; continuing after midnight \
                     Pacific time"
                        .to_owned(),
                )
            }
            "uploadLimitExceeded" => self.quota_paused(
                "the channel reached YouTube's daily upload limit; trying again after midnight \
                 Pacific time"
                    .to_owned(),
            ),
            "rateLimitExceeded" | "userRateLimitExceeded" => AdapterError::RateLimited {
                message: format!("YouTube asked to slow down: {message}"),
                retry_after: retry_after.unwrap_or(RATE_LIMIT_PAUSE),
            },
            _ if status == StatusCode::TOO_MANY_REQUESTS => AdapterError::RateLimited {
                message: format!("YouTube asked to slow down: {message}"),
                retry_after: retry_after.unwrap_or(RATE_LIMIT_PAUSE),
            },
            "authError" | "unauthorized" => AdapterError::Authentication { message },
            "insufficientPermissions" | "forbidden" | "accessNotConfigured"
                if status == StatusCode::FORBIDDEN =>
            {
                AdapterError::Authentication {
                    message: format!(
                        "YouTube refused the request ({message}); reconnect the channel and \
                         allow uploading and viewing your videos"
                    ),
                }
            }
            "youtubeSignupRequired" => AdapterError::Invalid {
                issues: vec!["the Google account has no YouTube channel yet".to_owned()],
            },
            _ if status.is_server_error() => AdapterError::Transient {
                message: format!("YouTube: {message}"),
                retry_after,
            },
            _ if status == StatusCode::BAD_REQUEST => AdapterError::Invalid {
                issues: vec![format!("YouTube rejected the video: {message}")],
            },
            _ => AdapterError::Permanent {
                message: format!("YouTube: {message}"),
            },
        }
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        response: Response,
        bucket: &str,
    ) -> Result<T, AdapterError> {
        if !response.status().is_success() {
            return Err(self.error(response, bucket).await);
        }
        response.json().await.map_err(|e| transport(&e))
    }

    async fn list_videos(
        &self,
        account: AccountId,
        ids: &[String],
        parts: &str,
    ) -> Result<Vec<Video>, AdapterError> {
        self.reserve(BUCKET_UNITS, youtube::COST_LIST, 0).await?;
        let url = self.api("/youtube/v3/videos");
        let id = ids.join(",");
        let response = self
            .send(account, |token| {
                self.http
                    .get(url.clone())
                    .query(&[("part", parts), ("id", id.as_str())])
                    .bearer_auth(token)
            })
            .await?;
        Ok(self
            .json::<Listing<Video>>(response, BUCKET_UNITS)
            .await?
            .items)
    }

    async fn video(
        &self,
        account: AccountId,
        id: &str,
        parts: &str,
    ) -> Result<Option<Video>, AdapterError> {
        Ok(self
            .list_videos(account, &[id.to_owned()], parts)
            .await?
            .into_iter()
            .find(|v| v.id == id))
    }

    /// The signed-in channel. Used when connecting.
    pub async fn my_channel(&self, account: AccountId) -> Result<ChannelInfo, AdapterError> {
        let channel = self.mine(account, "snippet").await?;
        Ok(ChannelInfo {
            title: channel
                .snippet
                .map(|s| s.title)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "YouTube channel".to_owned()),
            id: channel.id,
        })
    }

    async fn mine(&self, account: AccountId, part: &str) -> Result<Channel, AdapterError> {
        self.reserve(BUCKET_UNITS, youtube::COST_LIST, 0).await?;
        let url = self.api("/youtube/v3/channels");
        let response = self
            .send(account, |token| {
                self.http
                    .get(url.clone())
                    .query(&[("part", part), ("mine", "true")])
                    .bearer_auth(token)
            })
            .await?;
        self.json::<Listing<Channel>>(response, BUCKET_UNITS)
            .await?
            .items
            .into_iter()
            .next()
            .ok_or_else(|| AdapterError::Invalid {
                issues: vec!["the Google account has no YouTube channel yet".to_owned()],
            })
    }

    /// Looks for this app's upload among the channel's recent uploads by its marker tag.
    async fn find_by_marker(
        &self,
        account: AccountId,
        marker: &str,
    ) -> Result<Option<String>, AdapterError> {
        let uploads = self
            .mine(account, "contentDetails")
            .await?
            .content_details
            .map(|c| c.related_playlists.uploads)
            .unwrap_or_default();
        if uploads.is_empty() {
            return Ok(None);
        }
        self.reserve(BUCKET_UNITS, youtube::COST_LIST, 0).await?;
        let url = self.api("/youtube/v3/playlistItems");
        let depth = MARKER_SEARCH_DEPTH.to_string();
        let response = self
            .send(account, |token| {
                self.http
                    .get(url.clone())
                    .query(&[
                        ("part", "contentDetails"),
                        ("playlistId", uploads.as_str()),
                        ("maxResults", depth.as_str()),
                    ])
                    .bearer_auth(token)
            })
            .await?;
        let ids: Vec<String> = self
            .json::<Listing<PlaylistItem>>(response, BUCKET_UNITS)
            .await?
            .items
            .into_iter()
            .map(|i| i.content_details.video_id)
            .collect();
        if ids.is_empty() {
            return Ok(None);
        }
        Ok(self
            .list_videos(account, &ids, "snippet")
            .await?
            .into_iter()
            .find(|v| {
                v.snippet
                    .as_ref()
                    .is_some_and(|s| s.tags.iter().any(|t| t == marker))
            })
            .map(|v| v.id))
    }

    fn metadata(&self, s: &Submission<'_>, marker: &str, privacy: &str) -> Value {
        let content = s.publication.content();
        let mut tags = vec![marker.to_owned()];
        let mut chars = marker.len();
        for tag in &content.hashtags {
            // YouTube counts quoted tags with their quotes; stay well under its 500.
            let cost = tag.chars().count() + if tag.contains(' ') { 3 } else { 1 };
            if chars + cost > TAGS_MAX_CHARS {
                break;
            }
            chars += cost;
            tags.push(tag.clone());
        }
        json!({
            "snippet": {
                "title": youtube::title(&content.caption),
                "description": full_caption(&content.caption, &content.hashtags),
                "tags": tags,
                "categoryId": "22",
            },
            "status": {
                "privacyStatus": privacy,
                "selfDeclaredMadeForKids": false,
            },
        })
    }

    async fn open_session(&self, s: &Submission<'_>) -> Result<YouTubeUpload, AdapterError> {
        let account = s.account.id;
        if s.size_bytes == 0 {
            return Err(AdapterError::Invalid {
                issues: vec!["the rendered file is empty".to_owned()],
            });
        }
        // Leave enough of the other bucket to watch it process and confirm it.
        let day = quota_day((self.now)());
        let units = self.quota.used(BUCKET_UNITS, day).await.unwrap_or(0);
        if units.saturating_add(UNITS_PER_UPLOAD) > self.config.limits.units_per_day {
            return Err(self.quota_paused(format!(
                "today's YouTube API quota is almost used up ({units} of {} units); the upload \
                 starts after midnight Pacific time",
                self.config.limits.units_per_day
            )));
        }
        self.reserve(BUCKET_UPLOADS, youtube::COST_INSERT, 0)
            .await?;
        let privacy = if s.account.capabilities.private_only {
            "private"
        } else {
            "public"
        };
        let p = s.publication;
        let marker = marker(p.id(), p.revision().number);
        let body = self.metadata(s, &marker, privacy);
        let path = s.media_path.to_string_lossy().into_owned();
        let mime = youtube::mime_type(&path);
        let mut url = self.api("/upload/youtube/v3/videos");
        url.query_pairs_mut()
            .append_pair("uploadType", "resumable")
            .append_pair("part", "snippet,status");
        let size = s.size_bytes.to_string();
        let response = self
            .send(account, |token| {
                self.http
                    .post(url.clone())
                    .bearer_auth(token)
                    .header("X-Upload-Content-Length", size.as_str())
                    .header("X-Upload-Content-Type", mime)
                    .json(&body)
            })
            .await?;
        if !response.status().is_success() {
            return Err(self.error(response, BUCKET_UPLOADS).await);
        }
        let upload_url = response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Url::parse(v).ok())
            .filter(|u| u.scheme() == "https" || u.scheme() == "http")
            .ok_or_else(|| AdapterError::Transient {
                message: "YouTube did not return an upload session".to_owned(),
                retry_after: None,
            })?;
        Ok(YouTubeUpload {
            account_id: account,
            upload_url: upload_url.into(),
            offset: 0,
            total: s.size_bytes,
            marker,
            privacy: privacy.to_owned(),
            started_at: (self.now)(),
        })
    }

    async fn session_status(&self, cp: &YouTubeUpload) -> Result<SessionStatus, AdapterError> {
        let total = cp.total.to_string();
        let range = format!("bytes */{}", cp.total);
        let response = self
            .send(cp.account_id, |token| {
                self.http
                    .put(cp.upload_url.as_str())
                    .bearer_auth(token)
                    .header(CONTENT_LENGTH, "0")
                    .header(CONTENT_RANGE, range.as_str())
                    .header("X-Upload-Content-Length", total.as_str())
            })
            .await?;
        match response.status() {
            StatusCode::OK | StatusCode::CREATED => Ok(SessionStatus::Complete(
                response
                    .json::<Video>()
                    .await
                    .map_err(|e| transport(&e))?
                    .id,
            )),
            StatusCode::PERMANENT_REDIRECT => Ok(SessionStatus::Incomplete(received(&response))),
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(SessionStatus::Expired),
            _ => Err(self.error(response, BUCKET_UPLOADS).await),
        }
    }

    fn is_confirmed(&self, cp: &YouTubeUpload) -> bool {
        self.confirmed
            .lock()
            .expect("confirmed lock")
            .get(&cp.upload_url)
            .is_some_and(|o| *o == cp.offset)
    }

    fn confirm(&self, cp: &YouTubeUpload) {
        self.confirmed
            .lock()
            .expect("confirmed lock")
            .insert(cp.upload_url.clone(), cp.offset);
    }

    fn forget(&self, cp: &YouTubeUpload) {
        self.confirmed
            .lock()
            .expect("confirmed lock")
            .remove(&cp.upload_url);
    }

    fn media(&self, cp: &YouTubeUpload, id: String) -> YouTubeMedia {
        self.forget(cp);
        YouTubeMedia {
            id,
            account_id: cp.account_id,
            privacy: cp.privacy.clone(),
            uploaded_at: (self.now)(),
        }
    }

    async fn read_chunk(
        &self,
        s: &Submission<'_>,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, AdapterError> {
        let io = |e: std::io::Error| AdapterError::Permanent {
            message: format!("could not read the rendered video: {e}"),
        };
        let mut file = tokio::fs::File::open(s.media_path).await.map_err(io)?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io)?;
        let mut buf = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX)];
        file.read_exact(&mut buf).await.map_err(io)?;
        Ok(buf)
    }

    fn privacy_note(media: &YouTubeMedia, actual: &str, private_only: bool) -> Option<String> {
        match (media.privacy.as_str(), actual) {
            ("public", "private") => Some(KEPT_PRIVATE_NOTE.to_owned()),
            (_, "private") if private_only => Some(UPLOADED_PRIVATE_NOTE.to_owned()),
            (_, "private") => Some("Uploaded as private.".to_owned()),
            (_, "unlisted") => Some("Uploaded as unlisted.".to_owned()),
            _ => None,
        }
    }

    fn post(
        &self,
        media: &YouTubeMedia,
        privacy_status: String,
        note: Option<String>,
    ) -> PublishedPost<YouTubePost> {
        PublishedPost {
            remote_id: media.id.clone(),
            url: Some(youtube::shorts_url(&media.id)),
            note,
            post: YouTubePost {
                id: media.id.clone(),
                account_id: media.account_id,
                privacy_status,
            },
        }
    }
}

#[async_trait]
impl Adapter for YouTubeAdapter {
    type UploadCheckpoint = YouTubeUpload;
    type RemoteMedia = YouTubeMedia;
    type RemotePost = YouTubePost;

    fn platform(&self) -> Platform {
        Platform::YoutubeShorts
    }

    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError> {
        let now = (self.now)();
        if let Err(AdapterError::Authentication { message }) =
            self.tokens.access_token(account.id).await
        {
            return Ok(Capabilities::reconnect_required(
                &format!("Reconnect the channel to upload automatically ({message})."),
                now,
            ));
        }
        let limits = self.config.limits;
        let mut notes = Vec::new();
        if account.capabilities.private_only {
            notes.push(PRIVATE_ONLY_NOTE.to_owned());
        }
        notes.push(format!(
            "Quota: {} uploads and {} units a day for the Google Cloud project, reset at \
             midnight Pacific time. An upload costs 1 upload call plus a few units to watch it \
             process.",
            limits.uploads_per_day, limits.units_per_day
        ));
        notes.push(
            "While the Google consent screen is in Testing, the sign-in lasts 7 days; the app \
             asks you to reconnect when it runs out."
                .to_owned(),
        );
        notes.push(
            "Uploads are declared not made for kids. YouTube decides whether a video is a Short \
             (up to 3 minutes, square or vertical)."
                .to_owned(),
        );
        Ok(Capabilities {
            publish_access: PublishAccess::Automatic,
            native_scheduling: false,
            analytics: true,
            manual_handoff: true,
            private_only: account.capabilities.private_only,
            notes,
            checked_at: now,
        })
    }

    async fn validate(&self, s: &Submission<'_>) -> Result<Vec<ValidationIssue>, AdapterError> {
        let content = s.publication.content();
        let mut issues = Platform::YoutubeShorts.requirements().check(
            s.media,
            s.size_bytes,
            &content.caption,
            &content.hashtags,
        );
        let extension = youtube::extension(&s.media_path.to_string_lossy());
        if !youtube::FILE_EXTENSIONS.contains(&extension.as_str()) {
            issues.push(ValidationIssue::UnsupportedFormat { extension });
        }
        let description = full_caption(&content.caption, &content.hashtags);
        if description.len() > youtube::DESCRIPTION_MAX_BYTES {
            issues.retain(|i| !matches!(i, ValidationIssue::CaptionTooLong { .. }));
            issues.push(ValidationIssue::TooManyBytes {
                field: "description".to_owned(),
                bytes: description.len(),
                max: youtube::DESCRIPTION_MAX_BYTES,
            });
        }
        if youtube::forbidden_chars(&description) {
            issues.push(ValidationIssue::ForbiddenCharacters {
                field: "description".to_owned(),
            });
        }
        Ok(issues)
    }

    async fn begin_upload(&self, s: &Submission<'_>) -> Result<YouTubeUpload, AdapterError> {
        let cp = self.open_session(s).await?;
        self.confirm(&cp);
        Ok(cp)
    }

    async fn upload_step(
        &self,
        s: &Submission<'_>,
        mut cp: YouTubeUpload,
    ) -> Result<UploadProgress<YouTubeUpload, YouTubeMedia>, AdapterError> {
        if cp.total != s.size_bytes {
            return Err(AdapterError::Permanent {
                message: "the rendered video changed during the upload; publish it again".into(),
            });
        }
        if !self.is_confirmed(&cp) || cp.offset >= cp.total {
            match self.session_status(&cp).await? {
                SessionStatus::Complete(id) => {
                    return Ok(UploadProgress::Complete {
                        media: self.media(&cp, id),
                    });
                }
                SessionStatus::Incomplete(offset) => {
                    cp.offset = offset;
                    self.confirm(&cp);
                }
                SessionStatus::Expired => {
                    self.forget(&cp);
                    if let Some(id) = self.find_by_marker(cp.account_id, &cp.marker).await? {
                        return Ok(UploadProgress::Complete {
                            media: self.media(&cp, id),
                        });
                    }
                    let fresh = self.open_session(s).await?;
                    self.confirm(&fresh);
                    return Ok(UploadProgress::InProgress {
                        checkpoint: fresh,
                        fraction: 0.0,
                    });
                }
            }
        }

        let len = self.config.chunk_bytes.min(cp.total - cp.offset);
        let bytes = self.read_chunk(s, cp.offset, len).await?;
        let range = format!("bytes {}-{}/{}", cp.offset, cp.offset + len - 1, cp.total);
        let token = self.tokens.access_token(cp.account_id).await?;
        let sent = self
            .http
            .put(cp.upload_url.as_str())
            .bearer_auth(&token)
            .header(CONTENT_LENGTH, len)
            .header(CONTENT_RANGE, range)
            .header(
                CONTENT_TYPE,
                youtube::mime_type(&s.media_path.to_string_lossy()),
            )
            .body(bytes)
            .send()
            .await;
        let response = match sent {
            Ok(r) => r,
            Err(e) => {
                self.forget(&cp);
                return Err(transport(&e));
            }
        };
        match response.status() {
            StatusCode::PERMANENT_REDIRECT => {
                cp.offset = received(&response);
                self.confirm(&cp);
                #[allow(clippy::cast_precision_loss)]
                let fraction = cp.offset as f64 / cp.total as f64;
                Ok(UploadProgress::InProgress {
                    checkpoint: cp,
                    fraction,
                })
            }
            StatusCode::OK | StatusCode::CREATED => match response.json::<Video>().await {
                Ok(video) => Ok(UploadProgress::Complete {
                    media: self.media(&cp, video.id),
                }),
                Err(e) => {
                    // The bytes are in; the next step reads the result from the session.
                    self.forget(&cp);
                    Err(transport(&e))
                }
            },
            StatusCode::NOT_FOUND | StatusCode::GONE => {
                self.forget(&cp);
                Err(AdapterError::Transient {
                    message: "the YouTube upload session expired".into(),
                    retry_after: None,
                })
            }
            StatusCode::UNAUTHORIZED => {
                self.forget(&cp);
                self.tokens.invalidate(cp.account_id, &token).await;
                Err(AdapterError::Transient {
                    message: "YouTube asked for a fresh sign-in token".into(),
                    retry_after: None,
                })
            }
            _ => {
                self.forget(&cp);
                Err(self.error(response, BUCKET_UPLOADS).await)
            }
        }
    }

    async fn check_processing(&self, media: &YouTubeMedia) -> Result<Processing, AdapterError> {
        let Some(video) = self
            .video(media.account_id, &media.id, "status,processingDetails")
            .await?
        else {
            if (self.now)() - media.uploaded_at < LISTING_GRACE {
                return Ok(Processing::Pending {
                    poll_after: self.config.poll_every,
                    detail: Some("Waiting for YouTube to list the video…".into()),
                });
            }
            return Ok(Processing::Failed {
                message: "the video is no longer on the channel".into(),
            });
        };
        let status = video.status.unwrap_or_default();
        match status.upload_status.as_str() {
            "rejected" => {
                return Ok(Processing::Failed {
                    message: format!(
                        "YouTube rejected the video ({})",
                        status
                            .rejection_reason
                            .as_deref()
                            .unwrap_or("no reason given")
                    ),
                });
            }
            "failed" => {
                return Ok(Processing::Failed {
                    message: format!(
                        "YouTube could not process the video ({})",
                        status
                            .failure_reason
                            .as_deref()
                            .unwrap_or("no reason given")
                    ),
                });
            }
            "deleted" => {
                return Ok(Processing::Failed {
                    message: "the video was deleted on YouTube".into(),
                });
            }
            "processed" => return Ok(Processing::Ready),
            _ => {}
        }
        let details = video.processing_details.unwrap_or_default();
        match details.processing_status.as_str() {
            "succeeded" => Ok(Processing::Ready),
            "failed" | "terminated" => Ok(Processing::Failed {
                message: format!(
                    "YouTube could not process the video ({})",
                    details
                        .processing_failure_reason
                        .as_deref()
                        .unwrap_or(&details.processing_status)
                ),
            }),
            _ => {
                let progress = details.processing_progress.and_then(|p| {
                    let total: f64 = p.parts_total?.parse().ok()?;
                    let done: f64 = p.parts_processed?.parse().ok()?;
                    (total > 0.0).then(|| (done / total * 100.0).round())
                });
                Ok(Processing::Pending {
                    poll_after: self.config.poll_every,
                    detail: Some(match progress {
                        Some(pct) => format!("YouTube is processing the video ({pct}%)"),
                        None => "YouTube is processing the video".to_owned(),
                    }),
                })
            }
        }
    }

    async fn publish(
        &self,
        s: &Submission<'_>,
        media: &YouTubeMedia,
    ) -> Result<PublishedPost<YouTubePost>, AdapterError> {
        let video = self
            .video(media.account_id, &media.id, "status")
            .await?
            .ok_or_else(|| AdapterError::Permanent {
                message: "the uploaded video is no longer on the channel".into(),
            })?;
        let status = video.status.unwrap_or_default();
        if matches!(
            status.upload_status.as_str(),
            "rejected" | "failed" | "deleted"
        ) {
            return Err(AdapterError::Permanent {
                message: format!("YouTube reports the upload as {}", status.upload_status),
            });
        }
        let note = Self::privacy_note(
            media,
            &status.privacy_status,
            s.account.capabilities.private_only,
        );
        Ok(self.post(media, status.privacy_status, note))
    }

    async fn reconcile(
        &self,
        attempt: &PublishAttempt,
        media: Option<&YouTubeMedia>,
    ) -> Result<Reconciliation<YouTubePost>, AdapterError> {
        if let Some(media) = media {
            return Ok(
                match self.video(media.account_id, &media.id, "status").await? {
                    Some(video) => {
                        let privacy = video.status.unwrap_or_default().privacy_status;
                        let note = Self::privacy_note(media, &privacy, media.privacy == "private");
                        Reconciliation::Found {
                            post: self.post(media, privacy, note),
                        }
                    }
                    None => Reconciliation::NotFound,
                },
            );
        }
        // No video id recorded: ask the upload session, then look for the marker.
        let Some(cp) = attempt
            .upload_session
            .as_ref()
            .and_then(|v| serde_json::from_value::<YouTubeUpload>(v.clone()).ok())
        else {
            return Ok(Reconciliation::Unavailable {
                reason: "no YouTube upload was recorded for this attempt".into(),
            });
        };
        let found = match self.session_status(&cp).await? {
            SessionStatus::Complete(id) => Some(id),
            SessionStatus::Incomplete(_) => None,
            SessionStatus::Expired => self.find_by_marker(cp.account_id, &cp.marker).await?,
        };
        Ok(match found {
            Some(id) => {
                let media = YouTubeMedia {
                    id,
                    account_id: cp.account_id,
                    privacy: cp.privacy.clone(),
                    uploaded_at: (self.now)(),
                };
                Reconciliation::Found {
                    post: self.post(&media, cp.privacy.clone(), None),
                }
            }
            None => Reconciliation::Unavailable {
                reason: "YouTube shows no finished upload for this session and no recent \
                         video with its marker"
                    .into(),
            },
        })
    }

    async fn fetch_metrics(&self, post: &YouTubePost) -> Result<Vec<Metric>, AdapterError> {
        let Some(video) = self.video(post.account_id, &post.id, "statistics").await? else {
            return Ok(Vec::new());
        };
        let collected_at = (self.now)();
        let definitions = [
            (
                "viewCount",
                "views",
                "YouTube Data API statistics.viewCount: times the video was played; for Shorts every start or replay counts (since March 2025)",
            ),
            (
                "likeCount",
                "likes",
                "YouTube Data API statistics.likeCount",
            ),
            (
                "commentCount",
                "comments",
                "YouTube Data API statistics.commentCount",
            ),
        ];
        let stats = video.statistics.unwrap_or_default();
        Ok(definitions
            .iter()
            .filter_map(|(key, name, definition)| {
                let value: f64 = stats.get(*key)?.parse().ok()?;
                Some(Metric {
                    name: (*name).to_owned(),
                    value,
                    definition: (*definition).to_owned(),
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
    fn quota_days_follow_pacific_time() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        // 06:59 UTC on 30 October is still 29 October in California (PDT, UTC-7).
        assert_eq!(
            quota_day(at("2026-10-30T06:59:00Z")),
            NaiveDate::from_ymd_opt(2026, 10, 29).unwrap()
        );
        assert_eq!(
            quota_resets_at(at("2026-10-30T06:59:00Z")),
            at("2026-10-30T07:00:00Z")
        );
        // After the November change to PST (UTC-8), midnight is 08:00 UTC.
        assert_eq!(
            quota_resets_at(at("2026-11-10T12:00:00Z")),
            at("2026-11-11T08:00:00Z")
        );
    }

    #[test]
    fn markers_are_stable_and_short() {
        let id = PublicationId::new();
        assert_eq!(marker(id, 1), marker(id, 1));
        assert_ne!(marker(id, 1), marker(id, 2));
        assert_eq!(marker(id, 1).len(), 16);
    }

    #[tokio::test]
    async fn quota_report_warns_when_low() {
        let ledger = MemoryQuota::default();
        let now = Utc::now();
        let limits = QuotaLimits::default();
        let r = quota_report(&ledger, limits, now).await.unwrap();
        assert!(r.warning.is_none());
        ledger
            .spend(BUCKET_UPLOADS, quota_day(now), 85)
            .await
            .unwrap();
        let r = quota_report(&ledger, limits, now).await.unwrap();
        assert!(r.warning.unwrap().contains("running low"));
        ledger
            .exhaust(BUCKET_UNITS, quota_day(now), 10_000)
            .await
            .unwrap();
        let r = quota_report(&ledger, limits, now).await.unwrap();
        assert!(r.warning.unwrap().contains("used up"));
    }

    #[test]
    fn upload_debug_hides_the_session_url() {
        let cp = YouTubeUpload {
            account_id: AccountId::new(),
            upload_url: "https://upload.example/?upload_id=secret".into(),
            offset: 0,
            total: 1,
            marker: "sc1".into(),
            privacy: "private".into(),
            started_at: Utc::now(),
        };
        assert!(!format!("{cp:?}").contains("secret"));
    }
}
