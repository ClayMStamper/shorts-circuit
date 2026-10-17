//! TikTok as a *source*: the Display API (`user.info.basic`, `video.list`) is used only to
//! notice that a new post exists and to read its title, caption, cover, share link, post time
//! and duration. The Display API has no field for the original video file, and nothing here
//! downloads or scrapes video bytes: originals arrive through the watch folder.
//!
//! Access tokens come from the OAuth broker ([`crate::broker`]), which holds the client
//! secret. Tokens are sent as bearer headers and never logged.

use std::time::Duration;

use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The production Display API host. Tests point the client at a local fake instead.
pub const DISPLAY_API_BASE: &str = "https://open.tiktokapis.com";
/// The most posts `video.list` returns per page.
pub const MAX_PAGE_SIZE: u32 = 20;

const USER_FIELDS: &str = "open_id,display_name,avatar_url";
const VIDEO_FIELDS: &str =
    "id,title,video_description,duration,cover_image_url,share_url,create_time";

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum IntakeError {
    /// The access token is missing, expired or revoked, or a scope was not granted.
    #[error("TikTok needs you to connect again: {message}")]
    Unauthorized { message: String },
    #[error("TikTok is rate limiting requests")]
    RateLimited { retry_after: Option<Duration> },
    #[error("temporary problem reaching {service}: {message}")]
    Transient {
        service: &'static str,
        message: String,
    },
    #[error("{service} rejected the request ({code}): {message}")]
    Rejected {
        service: &'static str,
        code: String,
        message: String,
    },
}

impl IntakeError {
    pub(crate) fn from_reqwest(service: &'static str, e: &reqwest::Error) -> Self {
        // Don't include the URL: query strings may carry state values.
        let message = if e.is_timeout() {
            "timed out"
        } else if e.is_connect() {
            "could not connect"
        } else if e.is_decode() {
            "unexpected response"
        } else {
            "request failed"
        };
        Self::Transient {
            service,
            message: message.to_owned(),
        }
    }
}

/// The connected TikTok account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TikTokUser {
    pub open_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// One public post, as `video.list` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TikTokVideo {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The caption.
    #[serde(default)]
    pub video_description: Option<String>,
    /// Whole seconds.
    #[serde(default)]
    pub duration: Option<u32>,
    /// A signed CDN link that expires; copy the image if it should be kept.
    #[serde(default)]
    pub cover_image_url: Option<String>,
    #[serde(default)]
    pub share_url: Option<String>,
    /// Unix seconds, UTC.
    pub create_time: i64,
}

/// A page of posts, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPage {
    pub videos: Vec<TikTokVideo>,
    /// Pass back to get the next (older) page.
    pub cursor: Option<i64>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(default = "Option::default")]
    data: Option<T>,
    #[serde(default)]
    error: ApiError,
}

#[derive(Debug, Default, Deserialize)]
struct ApiError {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct UserData {
    user: TikTokUser,
}

#[derive(Debug, Deserialize)]
struct VideoData {
    #[serde(default)]
    videos: Vec<TikTokVideo>,
    #[serde(default)]
    cursor: Option<i64>,
    #[serde(default)]
    has_more: bool,
}

#[derive(Serialize)]
struct VideoListRequest {
    max_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<i64>,
}

/// Client for the parts of the Display API this app uses.
#[derive(Debug, Clone)]
pub struct DisplayApi {
    http: reqwest::Client,
    base: Url,
}

const SERVICE: &str = "TikTok";

impl DisplayApi {
    pub fn new(http: reqwest::Client, base: Url) -> Self {
        Self { http, base }
    }

    fn url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        url.set_path(path);
        url
    }

    /// `GET /v2/user/info/` (scope `user.info.basic`).
    pub async fn user_info(&self, access_token: &str) -> Result<TikTokUser, IntakeError> {
        let response = self
            .http
            .get(self.url("/v2/user/info/"))
            .query(&[("fields", USER_FIELDS)])
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        let data: UserData = read(response).await?;
        Ok(data.user)
    }

    /// `POST /v2/video/list/` (scope `video.list`): the user's public posts, newest first.
    pub async fn list_videos(
        &self,
        access_token: &str,
        cursor: Option<i64>,
        max_count: u32,
    ) -> Result<VideoPage, IntakeError> {
        let response = self
            .http
            .post(self.url("/v2/video/list/"))
            .query(&[("fields", VIDEO_FIELDS)])
            .bearer_auth(access_token)
            .json(&VideoListRequest {
                max_count: max_count.clamp(1, MAX_PAGE_SIZE),
                cursor,
            })
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        let data: VideoData = read(response).await?;
        Ok(VideoPage {
            videos: data.videos,
            cursor: data.cursor,
            has_more: data.has_more,
        })
    }
}

async fn read<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, IntakeError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = response
        .text()
        .await
        .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
    let envelope: Option<Envelope<T>> = serde_json::from_str(&body).ok();
    let (code, message) = envelope
        .as_ref()
        .map(|e| (e.error.code.clone(), e.error.message.clone()))
        .unwrap_or_default();
    let unauthorized = status == StatusCode::UNAUTHORIZED
        || matches!(
            code.as_str(),
            "access_token_invalid" | "scope_not_authorized" | "scope_permission_missed"
        );
    if unauthorized {
        return Err(IntakeError::Unauthorized {
            message: if message.is_empty() { code } else { message },
        });
    }
    if status == StatusCode::TOO_MANY_REQUESTS || code == "rate_limit_exceeded" {
        return Err(IntakeError::RateLimited { retry_after });
    }
    if status.is_server_error() || code == "internal_error" {
        return Err(IntakeError::Transient {
            service: SERVICE,
            message: format!("HTTP {}", status.as_u16()),
        });
    }
    match envelope {
        Some(Envelope {
            data: Some(data), ..
        }) if status.is_success() && (code.is_empty() || code == "ok") => Ok(data),
        _ => Err(IntakeError::Rejected {
            service: SERVICE,
            code: if code.is_empty() {
                format!("http_{}", status.as_u16())
            } else {
                code
            },
            message,
        }),
    }
}

/// Fetches a cover image (not a video) with a size cap. Returns the bytes and the file
/// extension implied by the content type.
pub async fn fetch_cover(
    http: &reqwest::Client,
    url: &str,
    max_bytes: usize,
) -> Result<(Vec<u8>, &'static str), IntakeError> {
    let url = Url::parse(url).map_err(|_| IntakeError::Rejected {
        service: SERVICE,
        code: "bad_cover_url".into(),
        message: "the cover link is not a URL".into(),
    })?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err(IntakeError::Rejected {
            service: SERVICE,
            code: "bad_cover_url".into(),
            message: "the cover link is not http(s)".into(),
        });
    }
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
    if !response.status().is_success() {
        return Err(IntakeError::Transient {
            service: SERVICE,
            message: format!("cover HTTP {}", response.status().as_u16()),
        });
    }
    let ext = match response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .as_deref()
    {
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("image/webp") => "webp",
        Some("image/heic") => "heic",
        _ => {
            return Err(IntakeError::Rejected {
                service: SERVICE,
                code: "not_an_image".into(),
                message: "the cover link did not return an image".into(),
            });
        }
    };
    if response
        .content_length()
        .is_some_and(|n| n > max_bytes as u64)
    {
        return Err(IntakeError::Rejected {
            service: SERVICE,
            code: "cover_too_large".into(),
            message: "the cover image is too large".into(),
        });
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
    if bytes.len() > max_bytes {
        return Err(IntakeError::Rejected {
            service: SERVICE,
            code: "cover_too_large".into(),
            message: "the cover image is too large".into(),
        });
    }
    Ok((bytes.to_vec(), ext))
}
