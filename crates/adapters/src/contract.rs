//! The adapter contract.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shorts_domain::{
    Capabilities, ConnectedAccount, MediaProperties, Platform, Publication, PublishAttempt,
    ValidationIssue,
};
use thiserror::Error;

/// Everything an adapter needs to know about what is being published.
#[derive(Debug, Clone, Copy)]
pub struct Submission<'a> {
    pub publication: &'a Publication,
    pub account: &'a ConnectedAccount,
    /// The rendered variant to upload.
    pub media_path: &'a Path,
    pub media: &'a MediaProperties,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UploadProgress<C, M> {
    /// More to send; persist `checkpoint` and call `upload_step` again.
    InProgress { checkpoint: C, fraction: f64 },
    /// The provider has the bytes.
    Complete { media: M },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Processing {
    Pending { poll_after: Duration },
    Ready,
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedPost<P> {
    pub remote_id: String,
    pub url: Option<String>,
    pub post: P,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reconciliation<P> {
    /// The post exists.
    Found { post: PublishedPost<P> },
    /// The provider confirms there is no such post.
    NotFound,
    /// The provider offers no way to tell; the user has to decide.
    Unavailable { reason: String },
}

/// A provider metric with its definition preserved, since definitions change over time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub definition: String,
    pub collected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AdapterError {
    #[error("temporary provider problem: {message}")]
    Transient {
        message: String,
        retry_after: Option<Duration>,
    },
    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after: Duration,
    },
    #[error("authentication required: {message}")]
    Authentication { message: String },
    #[error("the provider rejected the submission: {issues:?}")]
    Invalid { issues: Vec<String> },
    /// The request was sent but no response arrived; it may have taken effect.
    #[error("no response from the provider: {message}")]
    ResponseLost { message: String },
    #[error("not supported by this adapter: {message}")]
    Unsupported { message: String },
    #[error("provider error: {message}")]
    Permanent { message: String },
}

/// One platform's publishing integration. Upload, processing and publish are separate so
/// interrupted work resumes at the right step.
#[async_trait]
pub trait Adapter: Send + Sync + 'static {
    /// Upload session state persisted between `upload_step` calls.
    type UploadCheckpoint: Serialize + DeserializeOwned + Send + Sync;
    /// The provider's handle to uploaded media.
    type RemoteMedia: Serialize + DeserializeOwned + Send + Sync;
    /// The provider's handle to a published post.
    type RemotePost: Serialize + DeserializeOwned + Send + Sync;

    fn platform(&self) -> Platform;

    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError>;

    /// Checks the submission against the provider's current rules. An empty list means valid.
    async fn validate(
        &self,
        submission: &Submission<'_>,
    ) -> Result<Vec<ValidationIssue>, AdapterError>;

    async fn begin_upload(
        &self,
        submission: &Submission<'_>,
    ) -> Result<Self::UploadCheckpoint, AdapterError>;

    async fn upload_step(
        &self,
        submission: &Submission<'_>,
        checkpoint: Self::UploadCheckpoint,
    ) -> Result<UploadProgress<Self::UploadCheckpoint, Self::RemoteMedia>, AdapterError>;

    async fn check_processing(&self, media: &Self::RemoteMedia)
    -> Result<Processing, AdapterError>;

    async fn publish(
        &self,
        submission: &Submission<'_>,
        media: &Self::RemoteMedia,
    ) -> Result<PublishedPost<Self::RemotePost>, AdapterError>;

    /// After a lost publish response: find out whether the post exists.
    async fn reconcile(
        &self,
        attempt: &PublishAttempt,
        media: Option<&Self::RemoteMedia>,
    ) -> Result<Reconciliation<Self::RemotePost>, AdapterError>;

    async fn fetch_metrics(&self, post: &Self::RemotePost) -> Result<Vec<Metric>, AdapterError>;
}

/// An [`Adapter`] with its associated types erased to JSON, for storage in job checkpoints.
#[async_trait]
pub trait ErasedAdapter: Send + Sync {
    fn platform(&self) -> Platform;
    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError>;
    async fn validate(
        &self,
        submission: &Submission<'_>,
    ) -> Result<Vec<ValidationIssue>, AdapterError>;
    async fn begin_upload(&self, submission: &Submission<'_>) -> Result<Value, AdapterError>;
    async fn upload_step(
        &self,
        submission: &Submission<'_>,
        checkpoint: Value,
    ) -> Result<UploadProgress<Value, Value>, AdapterError>;
    async fn check_processing(&self, media: &Value) -> Result<Processing, AdapterError>;
    async fn publish(
        &self,
        submission: &Submission<'_>,
        media: &Value,
    ) -> Result<PublishedPost<Value>, AdapterError>;
    async fn reconcile(
        &self,
        attempt: &PublishAttempt,
        media: Option<&Value>,
    ) -> Result<Reconciliation<Value>, AdapterError>;
    async fn fetch_metrics(&self, post: &Value) -> Result<Vec<Metric>, AdapterError>;
}

fn to_json<T: Serialize>(value: &T) -> Result<Value, AdapterError> {
    serde_json::to_value(value).map_err(|e| AdapterError::Permanent {
        message: format!("could not serialize adapter state: {e}"),
    })
}

fn from_json<T: DeserializeOwned>(value: &Value, what: &str) -> Result<T, AdapterError> {
    serde_json::from_value(value.clone()).map_err(|e| AdapterError::Permanent {
        message: format!("stored {what} is not readable by this adapter version: {e}"),
    })
}

fn erase_post<P: Serialize>(post: PublishedPost<P>) -> Result<PublishedPost<Value>, AdapterError> {
    Ok(PublishedPost {
        remote_id: post.remote_id,
        url: post.url,
        post: to_json(&post.post)?,
    })
}

#[async_trait]
impl<A: Adapter> ErasedAdapter for A {
    fn platform(&self) -> Platform {
        Adapter::platform(self)
    }

    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError> {
        Adapter::capabilities(self, account).await
    }

    async fn validate(
        &self,
        submission: &Submission<'_>,
    ) -> Result<Vec<ValidationIssue>, AdapterError> {
        Adapter::validate(self, submission).await
    }

    async fn begin_upload(&self, submission: &Submission<'_>) -> Result<Value, AdapterError> {
        to_json(&Adapter::begin_upload(self, submission).await?)
    }

    async fn upload_step(
        &self,
        submission: &Submission<'_>,
        checkpoint: Value,
    ) -> Result<UploadProgress<Value, Value>, AdapterError> {
        let checkpoint = from_json(&checkpoint, "upload checkpoint")?;
        Ok(
            match Adapter::upload_step(self, submission, checkpoint).await? {
                UploadProgress::InProgress {
                    checkpoint,
                    fraction,
                } => UploadProgress::InProgress {
                    checkpoint: to_json(&checkpoint)?,
                    fraction,
                },
                UploadProgress::Complete { media } => UploadProgress::Complete {
                    media: to_json(&media)?,
                },
            },
        )
    }

    async fn check_processing(&self, media: &Value) -> Result<Processing, AdapterError> {
        let media = from_json(media, "remote media")?;
        Adapter::check_processing(self, &media).await
    }

    async fn publish(
        &self,
        submission: &Submission<'_>,
        media: &Value,
    ) -> Result<PublishedPost<Value>, AdapterError> {
        let media = from_json(media, "remote media")?;
        erase_post(Adapter::publish(self, submission, &media).await?)
    }

    async fn reconcile(
        &self,
        attempt: &PublishAttempt,
        media: Option<&Value>,
    ) -> Result<Reconciliation<Value>, AdapterError> {
        let media: Option<A::RemoteMedia> =
            media.map(|m| from_json(m, "remote media")).transpose()?;
        Ok(
            match Adapter::reconcile(self, attempt, media.as_ref()).await? {
                Reconciliation::Found { post } => Reconciliation::Found {
                    post: erase_post(post)?,
                },
                Reconciliation::NotFound => Reconciliation::NotFound,
                Reconciliation::Unavailable { reason } => Reconciliation::Unavailable { reason },
            },
        )
    }

    async fn fetch_metrics(&self, post: &Value) -> Result<Vec<Metric>, AdapterError> {
        let post = from_json(post, "remote post")?;
        Adapter::fetch_metrics(self, &post).await
    }
}
