//! An in-memory adapter for tests of code that drives the adapter contract. It never touches
//! the network.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use shorts_domain::{
    Capabilities, ConnectedAccount, Platform, PublishAccess, PublishAttempt, ValidationIssue,
};

use crate::contract::{
    Adapter, AdapterError, Metric, Processing, PublishedPost, Reconciliation, Submission,
    UploadProgress,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeUpload {
    pub session: String,
    pub sent: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeMedia {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakePost {
    pub id: String,
}

/// Which contract call a scripted error applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Validate,
    BeginUpload,
    UploadStep,
    CheckProcessing,
    Publish,
    Reconcile,
}

#[derive(Debug)]
pub struct FakeAdapter {
    pub platform: Platform,
    pub chunks: u32,
    pub processing_polls: Mutex<u32>,
    /// Errors returned (once each, in order) by the given call.
    pub script: Mutex<VecDeque<(Call, AdapterError)>>,
    /// A publish that "succeeds" on the provider but whose response is lost.
    pub lose_publish_response: Mutex<bool>,
    /// A publish that "succeeds" on the provider and then never returns, as if the process
    /// died mid-request.
    pub hang_after_publish: Mutex<bool>,
    pub calls: Mutex<Vec<Call>>,
    pub published: Mutex<Vec<FakePost>>,
}

impl FakeAdapter {
    pub fn new(platform: Platform, chunks: u32) -> Self {
        Self {
            platform,
            chunks,
            processing_polls: Mutex::new(1),
            script: Mutex::new(VecDeque::new()),
            lose_publish_response: Mutex::new(false),
            hang_after_publish: Mutex::new(false),
            calls: Mutex::new(Vec::new()),
            published: Mutex::new(Vec::new()),
        }
    }

    pub fn fail_next(&self, call: Call, error: AdapterError) {
        self.script.lock().unwrap().push_back((call, error));
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn count(&self, call: Call) -> usize {
        self.calls().iter().filter(|c| **c == call).count()
    }

    fn enter(&self, call: Call) -> Result<(), AdapterError> {
        self.calls.lock().unwrap().push(call);
        let mut script = self.script.lock().unwrap();
        if script.front().is_some_and(|(c, _)| *c == call) {
            return Err(script.pop_front().expect("checked").1);
        }
        Ok(())
    }
}

#[async_trait]
impl Adapter for FakeAdapter {
    type UploadCheckpoint = FakeUpload;
    type RemoteMedia = FakeMedia;
    type RemotePost = FakePost;

    fn platform(&self) -> Platform {
        self.platform
    }

    async fn capabilities(&self, account: &ConnectedAccount) -> Result<Capabilities, AdapterError> {
        Ok(Capabilities {
            publish_access: PublishAccess::Automatic,
            native_scheduling: false,
            analytics: false,
            manual_handoff: true,
            private_only: false,
            notes: Vec::new(),
            checked_at: account.updated_at,
        })
    }

    async fn validate(&self, s: &Submission<'_>) -> Result<Vec<ValidationIssue>, AdapterError> {
        self.enter(Call::Validate)?;
        let content = s.publication.content();
        Ok(self.platform.requirements().check(
            s.media,
            s.size_bytes,
            &content.caption,
            &content.hashtags,
        ))
    }

    async fn begin_upload(&self, s: &Submission<'_>) -> Result<FakeUpload, AdapterError> {
        self.enter(Call::BeginUpload)?;
        Ok(FakeUpload {
            session: format!("session-{}", s.publication.id()),
            sent: 0,
            total: self.chunks,
        })
    }

    async fn upload_step(
        &self,
        _s: &Submission<'_>,
        mut checkpoint: FakeUpload,
    ) -> Result<UploadProgress<FakeUpload, FakeMedia>, AdapterError> {
        self.enter(Call::UploadStep)?;
        checkpoint.sent += 1;
        if checkpoint.sent >= checkpoint.total {
            Ok(UploadProgress::Complete {
                media: FakeMedia {
                    id: format!("media-{}", checkpoint.session),
                },
            })
        } else {
            let fraction = f64::from(checkpoint.sent) / f64::from(checkpoint.total);
            Ok(UploadProgress::InProgress {
                checkpoint,
                fraction,
            })
        }
    }

    async fn check_processing(&self, _media: &FakeMedia) -> Result<Processing, AdapterError> {
        self.enter(Call::CheckProcessing)?;
        let mut polls = self.processing_polls.lock().unwrap();
        if *polls > 0 {
            *polls -= 1;
            return Ok(Processing::Pending {
                poll_after: Duration::from_millis(1),
                detail: Some("processing".into()),
            });
        }
        Ok(Processing::Ready)
    }

    async fn publish(
        &self,
        _s: &Submission<'_>,
        media: &FakeMedia,
    ) -> Result<PublishedPost<FakePost>, AdapterError> {
        self.enter(Call::Publish)?;
        let post = FakePost {
            id: format!("post-{}", media.id),
        };
        self.published.lock().unwrap().push(post.clone());
        if std::mem::take(&mut *self.hang_after_publish.lock().unwrap()) {
            std::future::pending::<()>().await;
        }
        if std::mem::take(&mut *self.lose_publish_response.lock().unwrap()) {
            return Err(AdapterError::ResponseLost {
                message: "connection closed before the response".into(),
            });
        }
        Ok(PublishedPost {
            remote_id: post.id.clone(),
            url: Some(format!("https://example.invalid/{}", post.id)),
            note: None,
            post,
        })
    }

    async fn reconcile(
        &self,
        _attempt: &PublishAttempt,
        media: Option<&FakeMedia>,
    ) -> Result<Reconciliation<FakePost>, AdapterError> {
        self.enter(Call::Reconcile)?;
        let Some(media) = media else {
            return Ok(Reconciliation::Unavailable {
                reason: "no remote media id recorded".into(),
            });
        };
        let id = format!("post-{}", media.id);
        let found = self.published.lock().unwrap().iter().any(|p| p.id == id);
        Ok(if found {
            Reconciliation::Found {
                post: PublishedPost {
                    remote_id: id.clone(),
                    url: Some(format!("https://example.invalid/{id}")),
                    note: None,
                    post: FakePost { id },
                },
            }
        } else {
            Reconciliation::NotFound
        })
    }

    async fn fetch_metrics(&self, _post: &FakePost) -> Result<Vec<Metric>, AdapterError> {
        Ok(Vec::new())
    }
}
