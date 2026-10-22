//! The LinkedIn adapter against a local fake of LinkedIn's REST API and upload addresses
//! (wiremock). Nothing here calls LinkedIn.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;
use shorts_adapters::linkedin::{
    LiSession, LinkedInAdapter, LinkedInAuth, LinkedInConfig, LinkedInMedia, LinkedInUpload,
};
use shorts_adapters::{
    Adapter, AdapterError, Processing, Reconciliation, Submission, UploadProgress,
};
use shorts_domain::{
    AccountId, AssetId, Capabilities, ConnectedAccount, Destination, MediaProperties, Platform,
    Publication, PublishAccess, PublishAttempt, RevisionContent, ValidationIssue, VariantId,
};
use tempfile::TempDir;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SIZE: u64 = 200 * 1024;
const HALF: u64 = 100 * 1024;
const ORG: &str = "urn:li:organization:4242";
const VIDEO: &str = "urn:li:video:C5F10AQtest";
const VIDEO_PATH: &str = "/rest/videos/urn%3Ali%3Avideo%3AC5F10AQtest";

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-30T18:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[derive(Default)]
struct Tokens {
    queue: Mutex<VecDeque<String>>,
    current: Mutex<Option<String>>,
    invalidated: Mutex<Vec<String>>,
}

impl Tokens {
    fn new(tokens: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(tokens.iter().map(|t| (*t).to_owned()).collect()),
            ..Default::default()
        })
    }
}

#[async_trait]
impl LinkedInAuth for Tokens {
    async fn session(&self, _account: AccountId) -> Result<LiSession, AdapterError> {
        let mut current = self.current.lock().unwrap();
        if current.is_none() {
            *current = self.queue.lock().unwrap().pop_front();
        }
        current
            .clone()
            .map(|access_token| LiSession {
                access_token,
                author: ORG.into(),
            })
            .ok_or_else(|| AdapterError::Authentication {
                message: "the token expired".into(),
            })
    }

    async fn invalidate(&self, _account: AccountId, token: &str) {
        self.invalidated.lock().unwrap().push(token.to_owned());
        *self.current.lock().unwrap() = None;
    }
}

struct Fixture {
    server: MockServer,
    dir: TempDir,
    file: PathBuf,
    account: ConnectedAccount,
    publication: Publication,
    props: MediaProperties,
}

impl Fixture {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clip.mp4");
        let bytes: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, bytes).unwrap();
        let mut account = ConnectedAccount::manual(Platform::Linkedin, "Timeless Way", t0());
        account.capabilities = Capabilities {
            publish_access: PublishAccess::Automatic,
            ..Capabilities::manual_only(t0())
        };
        let content = RevisionContent::new(
            VariantId::new(),
            1,
            "New patch (beta) is out!\nPlaytest on itch",
            &["gamedev".into(), "indie-dev".into()],
            Destination {
                account_id: account.id,
                platform: account.platform,
            },
        )
        .unwrap();
        let publication = Publication::draft(AssetId::new(), content, None, t0());
        let props = MediaProperties {
            duration_ms: 35_000,
            width: 1080,
            height: 1920,
            audio_streams: 1,
            video_codec: Some("h264".into()),
            ..Default::default()
        };
        Self {
            server,
            dir,
            file,
            account,
            publication,
            props,
        }
    }

    fn adapter(&self, tokens: Arc<dyn LinkedInAuth>) -> LinkedInAdapter {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        LinkedInAdapter::new(
            http,
            LinkedInConfig {
                api_base: self.server.uri().parse().unwrap(),
                api_version: "202609".into(),
                poll_every: Duration::from_millis(1),
                part_timeout: Duration::from_millis(500),
            },
            tokens,
            Arc::new(t0),
        )
    }

    fn submission(&self) -> Submission<'_> {
        Submission {
            publication: &self.publication,
            account: &self.account,
            media_path: &self.file,
            media: &self.props,
            size_bytes: SIZE,
        }
    }

    fn media(&self) -> LinkedInMedia {
        LinkedInMedia {
            id: VIDEO.into(),
            account_id: self.account.id,
            owner: ORG.into(),
            uploaded_at: t0(),
        }
    }

    async fn mock_initialize(&self, video: &str, part_prefix: &str) {
        let base = self.server.uri();
        Mock::given(method("POST"))
            .and(path("/rest/videos"))
            .and(query_param("action", "initializeUpload"))
            .and(header("LinkedIn-Version", "202609"))
            .and(header("X-Restli-Protocol-Version", "2.0.0"))
            .and(header("authorization", "Bearer AQV-one"))
            .and(body_partial_json(json!({
                "initializeUploadRequest": { "owner": ORG, "fileSizeBytes": SIZE }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": {
                    "video": video,
                    "uploadToken": "",
                    "uploadUrlsExpireAt": 1_798_000_000_000_u64,
                    "uploadInstructions": [
                        { "uploadUrl": format!("{base}/{part_prefix}/2"), "firstByte": HALF, "lastByte": SIZE - 1 },
                        { "uploadUrl": format!("{base}/{part_prefix}/1"), "firstByte": 0, "lastByte": HALF - 1 },
                    ]
                }
            })))
            .up_to_n_times(1)
            .mount(&self.server)
            .await;
    }

    async fn mock_parts(&self, prefix: &str) {
        for (n, tag) in [(1, "etag-1"), (2, "etag-2")] {
            Mock::given(method("PUT"))
                .and(path(format!("/{prefix}/{n}")))
                .and(header("content-type", "application/octet-stream"))
                .and(header("content-length", HALF.to_string().as_str()))
                .respond_with(ResponseTemplate::new(201).insert_header("ETag", tag))
                .mount(&self.server)
                .await;
        }
    }

    async fn mock_finalize(&self, video: &str) {
        Mock::given(method("POST"))
            .and(path("/rest/videos"))
            .and(query_param("action", "finalizeUpload"))
            .and(body_partial_json(json!({
                "finalizeUploadRequest": { "video": video, "uploadedPartIds": ["etag-1", "etag-2"] }
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&self.server)
            .await;
    }

    async fn mock_recent_posts(&self, media_id: &str) {
        Mock::given(method("GET"))
            .and(path("/rest/posts"))
            .and(query_param("q", "author"))
            .and(query_param("author", ORG))
            .and(header("X-RestLi-Method", "FINDER"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "elements": [
                    { "id": "urn:li:share:1", "content": { "article": { "source": "https://x" } } },
                    { "id": "urn:li:share:9", "content": { "media": { "id": media_id, "title": "t" } } },
                ]
            })))
            .mount(&self.server)
            .await;
    }
}

async fn upload_all(
    a: &LinkedInAdapter,
    s: &Submission<'_>,
    mut cp: LinkedInUpload,
) -> LinkedInMedia {
    for _ in 0..10 {
        match a.upload_step(s, cp).await.unwrap() {
            UploadProgress::InProgress { checkpoint, .. } => cp = checkpoint,
            UploadProgress::Complete { media } => return media,
        }
    }
    panic!("the upload never finished");
}

#[tokio::test]
async fn a_video_is_uploaded_in_parts_and_posted_as_the_page() {
    let f = Fixture::new().await;
    f.mock_initialize(VIDEO, "part").await;
    f.mock_parts("part").await;
    f.mock_finalize(VIDEO).await;
    Mock::given(method("GET"))
        .and(path(VIDEO_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "PROCESSING" })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path(VIDEO_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "AVAILABLE" })))
        .mount(&f.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/posts"))
        .and(header("LinkedIn-Version", "202609"))
        .and(body_partial_json(json!({
            "author": ORG,
            "commentary": "New patch \\(beta\\) is out!\nPlaytest on itch\n\n{hashtag|\\#|gamedev} {hashtag|\\#|indiedev}",
            "visibility": "PUBLIC",
            "lifecycleState": "PUBLISHED",
            "content": { "media": { "id": VIDEO, "title": "New patch (beta) is out!" } },
        })))
        .respond_with(ResponseTemplate::new(201).insert_header("x-restli-id", "urn:li:share:9"))
        .expect(1)
        .mount(&f.server)
        .await;

    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let s = f.submission();
    assert!(a.validate(&s).await.unwrap().is_empty());
    let cp = a.begin_upload(&s).await.unwrap();
    assert_eq!(cp.parts.len(), 2);
    assert_eq!(cp.parts[0].first_byte, 0, "parts are sorted");
    let UploadProgress::InProgress {
        checkpoint,
        fraction,
    } = a.upload_step(&s, cp).await.unwrap()
    else {
        panic!("two parts to send");
    };
    assert_eq!(fraction, 0.5);
    let media = upload_all(&a, &s, checkpoint).await;
    assert_eq!(media.id, VIDEO);
    assert!(matches!(
        a.check_processing(&media).await.unwrap(),
        Processing::Pending { .. }
    ));
    assert_eq!(a.check_processing(&media).await.unwrap(), Processing::Ready);
    let post = a.publish(&s, &media).await.unwrap();
    assert_eq!(post.remote_id, "urn:li:share:9");
    assert_eq!(
        post.url.as_deref(),
        Some("https://www.linkedin.com/feed/update/urn:li:share:9/")
    );
}

#[tokio::test]
async fn a_restart_resumes_at_the_next_part() {
    let f = Fixture::new().await;
    f.mock_initialize(VIDEO, "part").await;
    f.mock_parts("part").await;
    f.mock_finalize(VIDEO).await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    let UploadProgress::InProgress { checkpoint, .. } = a.upload_step(&s, cp).await.unwrap() else {
        panic!("one part left");
    };
    // A new process reads the checkpoint back from the job.
    let stored: LinkedInUpload =
        serde_json::from_value(serde_json::to_value(&checkpoint).unwrap()).unwrap();
    assert_eq!(stored.etags, vec!["etag-1"]);
    let fresh = f.adapter(Tokens::new(&["AQV-one"]));
    let UploadProgress::Complete { media } = fresh.upload_step(&s, stored).await.unwrap() else {
        panic!("the last part finishes the upload");
    };
    assert_eq!(media.id, VIDEO);
    let first_part_puts = f
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/part/1")
        .count();
    assert_eq!(first_part_puts, 1, "the first part isn't sent again");
}

#[tokio::test]
async fn a_refused_part_address_starts_a_new_upload() {
    let f = Fixture::new().await;
    f.mock_initialize(VIDEO, "old").await;
    Mock::given(method("PUT"))
        .and(path("/old/1"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    f.mock_initialize("urn:li:video:second", "new").await;
    let UploadProgress::InProgress {
        checkpoint,
        fraction,
    } = a.upload_step(&s, cp).await.unwrap()
    else {
        panic!("expected a fresh upload");
    };
    assert_eq!(checkpoint.video, "urn:li:video:second");
    assert!(checkpoint.upload_url.ends_with("/new/1"));
    assert!(checkpoint.etags.is_empty());
    assert_eq!(fraction, 0.0);
}

#[tokio::test]
async fn a_failing_part_is_retried_later() {
    let f = Fixture::new().await;
    f.mock_initialize(VIDEO, "part").await;
    Mock::given(method("PUT"))
        .and(path("/part/1"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    let err = a.upload_step(&s, cp).await.unwrap_err();
    assert!(matches!(err, AdapterError::Transient { .. }), "{err:?}");
}

#[tokio::test]
async fn refused_tokens_and_permissions_ask_for_a_reconnect() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/rest/videos"))
        .and(header("authorization", "Bearer AQV-old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "status": 401, "serviceErrorCode": 65601, "code": "REVOKED_ACCESS_TOKEN",
            "message": "The token used in the request has been revoked by the user"
        })))
        .mount(&f.server)
        .await;
    let tokens = Tokens::new(&["AQV-old"]);
    let a = f.adapter(tokens.clone());
    let err = a.begin_upload(&f.submission()).await.unwrap_err();
    assert!(
        matches!(err, AdapterError::Authentication { .. }),
        "{err:?}"
    );
    assert_eq!(*tokens.invalidated.lock().unwrap(), vec!["AQV-old"]);

    Mock::given(method("POST"))
        .and(path("/rest/videos"))
        .and(header("authorization", "Bearer AQV-one"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "status": 403, "serviceErrorCode": 100, "code": "ACCESS_DENIED",
            "message": "Not enough permissions to access: videos.ACTION.initializeUpload"
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let AdapterError::Authentication { message } =
        a.begin_upload(&f.submission()).await.unwrap_err()
    else {
        panic!("expected a reconnect");
    };
    assert!(message.contains("w_organization_social"));
}

#[tokio::test]
async fn daily_limits_wait_until_midnight_utc() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/rest/videos"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "status": 429, "message": "Resource level throttle limit"
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let AdapterError::RateLimited { retry_after, .. } =
        a.begin_upload(&f.submission()).await.unwrap_err()
    else {
        panic!("expected a rate-limit pause");
    };
    // 18:00 UTC → midnight is 6 hours away, plus a small margin.
    assert_eq!(retry_after, Duration::from_secs(6 * 3600 + 120));
}

#[tokio::test]
async fn an_unsupported_api_version_is_reported_plainly() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/rest/videos"))
        .respond_with(ResponseTemplate::new(426).set_body_json(json!({
            "status": 426, "code": "NONEXISTENT_VERSION", "message": "Requested version 20260900 is not active"
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let AdapterError::Permanent { message } = a.begin_upload(&f.submission()).await.unwrap_err()
    else {
        panic!("expected a permanent error");
    };
    assert!(message.contains("SHORTS_LINKEDIN_API_VERSION"));
}

#[tokio::test]
async fn a_post_without_an_id_is_reconciled_by_its_video() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/rest/posts"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&f.server)
        .await;
    f.mock_recent_posts(VIDEO).await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let media = f.media();
    let err = a.publish(&f.submission(), &media).await.unwrap_err();
    assert!(matches!(err, AdapterError::ResponseLost { .. }), "{err:?}");
    let attempt = PublishAttempt::start(f.publication.id(), 1, None, t0());
    let Reconciliation::Found { post } = a.reconcile(&attempt, Some(&media)).await.unwrap() else {
        panic!("the post exists");
    };
    assert_eq!(post.remote_id, "urn:li:share:9");
}

#[tokio::test]
async fn reconcile_reports_missing_and_unreadable_posts() {
    let f = Fixture::new().await;
    f.mock_recent_posts("urn:li:video:someOtherVideo").await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let attempt = PublishAttempt::start(f.publication.id(), 1, None, t0());
    assert_eq!(
        a.reconcile(&attempt, Some(&f.media())).await.unwrap(),
        Reconciliation::NotFound
    );
    assert!(matches!(
        a.reconcile(&attempt, None).await.unwrap(),
        Reconciliation::Unavailable { .. }
    ));

    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/rest/posts"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "message": "no r_organization_social" })),
        )
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    assert!(matches!(
        a.reconcile(&attempt, Some(&f.media())).await.unwrap(),
        Reconciliation::Unavailable { .. }
    ));
}

#[tokio::test]
async fn processing_failures_are_reported() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path(VIDEO_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "status": "PROCESSING_FAILED" })),
        )
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    assert!(matches!(
        a.check_processing(&f.media()).await.unwrap(),
        Processing::Failed { .. }
    ));
}

#[tokio::test]
async fn the_pages_a_token_administers_are_listed_by_name() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/rest/organizationAcls"))
        .and(query_param("q", "roleAssignee"))
        .and(query_param("role", "ADMINISTRATOR"))
        .and(header("authorization", "Bearer AQV-pasted"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "elements": [
                { "organization": ORG, "role": "ADMINISTRATOR", "state": "APPROVED" },
                { "organization": "urn:li:organization:7", "role": "ADMINISTRATOR", "state": "APPROVED" },
            ]
        })))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/organizations/4242"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "localizedName": "Timeless Way Interactive" })),
        )
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/organizations/7"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&[]));
    let orgs = a.organizations("AQV-pasted").await.unwrap();
    assert_eq!(orgs.len(), 2);
    assert_eq!(orgs[0].urn, ORG);
    assert_eq!(orgs[0].name, "Timeless Way Interactive");
    assert_eq!(orgs[1].name, "LinkedIn page 7");
}

#[tokio::test]
async fn validation_follows_linkedin_rules() {
    let mut f = Fixture::new().await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    f.props.width = 1920;
    f.props.height = 1080;
    assert!(a.validate(&f.submission()).await.unwrap().is_empty());
    let mov = f.dir.path().join("clip.mov");
    std::fs::copy(&f.file, &mov).unwrap();
    f.file = mov;
    let issues = a.validate(&f.submission()).await.unwrap();
    assert!(matches!(
        &issues[..],
        [ValidationIssue::UnsupportedFormat { extension }] if extension == "mov"
    ));
}

#[tokio::test]
async fn capabilities_name_the_page_and_ask_for_a_reconnect_without_a_token() {
    let f = Fixture::new().await;
    let a = f.adapter(Tokens::new(&["AQV-one"]));
    let c = a.capabilities(&f.account).await.unwrap();
    assert!(c.can_publish_automatically());
    assert!(c.notes[0].contains("Timeless Way"));
    let a = f.adapter(Tokens::new(&[]));
    assert!(
        !a.capabilities(&f.account)
            .await
            .unwrap()
            .can_publish_automatically()
    );
}
