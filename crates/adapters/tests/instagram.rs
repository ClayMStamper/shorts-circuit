//! The Instagram adapter against a local fake of the Graph API (`graph.facebook.com`, Facebook
//! Login) and its upload host (wiremock). Nothing here calls Meta.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;
use shorts_adapters::instagram::{
    IgSession, InstagramAdapter, InstagramAuth, InstagramConfig, InstagramMedia, InstagramUpload,
    caption_digest,
};
use shorts_adapters::{
    Adapter, AdapterError, Processing, Reconciliation, Submission, UploadProgress,
};
use shorts_domain::platform::full_caption;
use shorts_domain::{
    AccountId, AssetId, Capabilities, ConnectedAccount, Destination, MediaProperties, Platform,
    Publication, PublishAccess, PublishAttempt, RevisionContent, ValidationIssue, VariantId,
};
use tempfile::TempDir;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SIZE: usize = 200 * 1024;
const USER: &str = "17841400000000001";
const CAPTION: &str = "Crispy edges every time\nFull recipe in bio";

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-30T18:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// Hands out tokens in order; `invalidate` makes the next call take the next one.
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
impl InstagramAuth for Tokens {
    async fn session(&self, _account: AccountId) -> Result<IgSession, AdapterError> {
        let mut current = self.current.lock().unwrap();
        if current.is_none() {
            *current = self.queue.lock().unwrap().pop_front();
        }
        current
            .clone()
            .map(|access_token| IgSession {
                access_token,
                user_id: USER.into(),
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
    _dir: TempDir,
    file: PathBuf,
    account: ConnectedAccount,
    publication: Publication,
    props: MediaProperties,
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl Fixture {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("reel.mp4");
        let bytes: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, bytes).unwrap();
        let mut account = ConnectedAccount::manual(Platform::InstagramReels, "@kitchen", t0());
        account.capabilities = Capabilities {
            publish_access: PublishAccess::Automatic,
            ..Capabilities::manual_only(t0())
        };
        let content = RevisionContent::new(
            VariantId::new(),
            1,
            CAPTION,
            &["bread".into(), "baking".into()],
            Destination {
                account_id: account.id,
                platform: account.platform,
            },
        )
        .unwrap();
        let publication = Publication::draft(AssetId::new(), content, None, t0());
        let props = MediaProperties {
            duration_ms: 20_000,
            width: 1080,
            height: 1920,
            audio_streams: 1,
            video_codec: Some("h264".into()),
            ..Default::default()
        };
        Self {
            server,
            _dir: dir,
            file,
            account,
            publication,
            props,
            now: Arc::new(Mutex::new(t0())),
        }
    }

    fn adapter(&self, tokens: Arc<dyn InstagramAuth>) -> InstagramAdapter {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let now = self.now.clone();
        InstagramAdapter::new(
            http,
            InstagramConfig {
                graph_base: self.server.uri().parse().unwrap(),
                instagram_login_base: format!("{}/ig", self.server.uri()).parse().unwrap(),
                upload_base: format!("{}/rupload", self.server.uri()).parse().unwrap(),
                api_version: "v24.0".into(),
                poll_every: Duration::from_millis(1),
                upload_timeout: Duration::from_millis(500),
                slowest_uplink: u64::MAX,
            },
            tokens,
            Arc::new(move || *now.lock().unwrap()),
        )
    }

    fn submission(&self) -> Submission<'_> {
        Submission {
            publication: &self.publication,
            account: &self.account,
            media_path: &self.file,
            media: &self.props,
            size_bytes: SIZE as u64,
        }
    }

    fn caption(&self) -> String {
        full_caption(CAPTION, &["bread".into(), "baking".into()])
    }

    fn media(&self) -> InstagramMedia {
        InstagramMedia {
            id: "c1".into(),
            account_id: self.account.id,
            caption_digest: caption_digest(&self.caption()),
            uploaded_at: t0(),
        }
    }

    async fn mock_container(&self) {
        Mock::given(method("POST"))
            .and(path(format!("/v24.0/{USER}/media")))
            .and(header("authorization", "Bearer EAA-page-one"))
            .and(body_string_contains("media_type=REELS"))
            .and(body_string_contains("upload_type=resumable"))
            .and(body_string_contains("share_to_feed=true"))
            .and(body_string_contains("caption=Crispy+edges"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "uri": format!("{}/ig-api-upload/v24.0/c1", self.server.uri()),
            })))
            .mount(&self.server)
            .await;
    }

    async fn mock_status(&self, code: &str, times: Option<u64>) {
        let mock = Mock::given(method("GET"))
            .and(path("/v24.0/c1"))
            .and(query_param("fields", "status_code,status"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "status_code": code, "id": "c1" })),
            );
        match times {
            Some(n) => mock.up_to_n_times(n).with_priority(1),
            None => mock,
        }
        .mount(&self.server)
        .await;
    }

    /// What the container reports about the upload so far.
    async fn mock_received(&self, bytes: Option<u64>, times: Option<u64>) {
        let body = match bytes {
            Some(b) => json!({ "id": "c1", "video_status": {
                "uploading_phase": { "status": "in_progress", "bytes_transferred": b },
                "processing_phase": { "status": "not_started" },
            }}),
            None => json!({ "id": "c1" }),
        };
        let mock = Mock::given(method("GET"))
            .and(path("/v24.0/c1"))
            .and(query_param("fields", "video_status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body));
        match times {
            Some(n) => mock.up_to_n_times(n).with_priority(1),
            None => mock,
        }
        .mount(&self.server)
        .await;
    }

    async fn mock_limit(&self, usage: u32) {
        Mock::given(method("GET"))
            .and(path(format!("/v24.0/{USER}/content_publishing_limit")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "quota_usage": usage, "config": { "quota_total": 100, "quota_duration": 86400 } }]
            })))
            .mount(&self.server)
            .await;
    }

    async fn mock_permalink(&self) {
        Mock::given(method("GET"))
            .and(path("/v24.0/m1"))
            .and(query_param("fields", "permalink,shortcode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "m1",
                "permalink": "https://www.instagram.com/reel/AbC123/",
                "shortcode": "AbC123",
            })))
            .mount(&self.server)
            .await;
    }
}

fn graph_error(status: u16, code: i64, subcode: Option<i64>) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "error": {
            "message": "test error",
            "type": "OAuthException",
            "code": code,
            "error_subcode": subcode,
            "fbtrace_id": "x",
        }
    }))
}

#[tokio::test]
async fn a_reel_is_uploaded_processed_and_published() {
    let f = Fixture::new().await;
    f.mock_container().await;
    f.mock_received(Some(0), None).await;
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .and(header("authorization", "OAuth EAA-page-one"))
        .and(header("offset", "0"))
        .and(header("file_size", SIZE.to_string().as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "success": true, "message": "Upload successful." })),
        )
        .expect(1)
        .mount(&f.server)
        .await;
    f.mock_status("IN_PROGRESS", Some(1)).await;
    f.mock_status("FINISHED", None).await;
    f.mock_limit(3).await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media_publish")))
        .and(body_string_contains("creation_id=c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "m1" })))
        .expect(1)
        .mount(&f.server)
        .await;
    f.mock_permalink().await;

    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let s = f.submission();
    assert!(a.validate(&s).await.unwrap().is_empty());
    let cp = a.begin_upload(&s).await.unwrap();
    assert_eq!(cp.container_id, "c1");
    assert_eq!(cp.caption_digest, caption_digest(&f.caption()));
    let UploadProgress::Complete { media } = a.upload_step(&s, cp).await.unwrap() else {
        panic!("one request uploads the whole reel");
    };
    assert_eq!(media.id, "c1");
    assert!(matches!(
        a.check_processing(&media).await.unwrap(),
        Processing::Pending { .. }
    ));
    assert_eq!(a.check_processing(&media).await.unwrap(), Processing::Ready);
    let post = a.publish(&s, &media).await.unwrap();
    assert_eq!(post.remote_id, "m1");
    assert_eq!(
        post.url.as_deref(),
        Some("https://www.instagram.com/reel/AbC123/")
    );
}

#[tokio::test]
async fn a_container_without_an_upload_address_uses_the_default_host() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "c9" })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let cp = a.begin_upload(&f.submission()).await.unwrap();
    assert_eq!(
        cp.upload_url,
        format!("{}/rupload/ig-api-upload/v24.0/c9", f.server.uri())
    );
}

#[tokio::test]
async fn a_workflow_can_keep_reels_off_the_main_feed() {
    let mut f = Fixture::new().await;
    let mut content = f.publication.content().clone();
    content.options.share_to_feed = Some(false);
    f.publication = Publication::draft(AssetId::new(), content, None, t0());
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media")))
        .and(body_string_contains("share_to_feed=false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "c8" })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let cp = a.begin_upload(&f.submission()).await.unwrap();
    assert_eq!(cp.container_id, "c8");
}

#[tokio::test]
async fn an_interrupted_upload_resumes_from_the_bytes_instagram_has() {
    let f = Fixture::new().await;
    f.mock_container().await;
    let half = (SIZE / 2) as u64;
    // First step: nothing has arrived yet, and the upload breaks.
    f.mock_received(Some(0), Some(1)).await;
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .and(header("offset", "0"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": { "message": "try again", "code": 2, "is_transient": true }
        })))
        .expect(1)
        .mount(&f.server)
        .await;
    // Then Instagram reports half the file and only the rest is sent.
    f.mock_received(Some(half), None).await;
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .and(header("offset", half.to_string().as_str()))
        .and(header("file_size", SIZE.to_string().as_str()))
        .and(header("authorization", "OAuth EAA-page-one"))
        .respond_with(move |req: &wiremock::Request| {
            assert_eq!(req.body.len(), SIZE / 2, "only the missing half is sent");
            assert_eq!(
                req.body[0],
                (SIZE / 2 % 251) as u8,
                "starting at the offset"
            );
            ResponseTemplate::new(200).set_body_json(json!({ "success": true }))
        })
        .expect(1)
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    let err = a.upload_step(&s, cp.clone()).await.unwrap_err();
    assert!(matches!(err, AdapterError::Transient { .. }), "{err:?}");
    // A restart resumes from the stored checkpoint (written before `offset` existed too).
    let mut raw = serde_json::to_value(&cp).unwrap();
    raw.as_object_mut().unwrap().remove("offset");
    let stored: InstagramUpload = serde_json::from_value(raw).unwrap();
    assert!(matches!(
        a.upload_step(&s, stored).await.unwrap(),
        UploadProgress::Complete { .. }
    ));
}

#[tokio::test]
async fn an_upload_whose_answer_was_lost_is_not_sent_again() {
    let f = Fixture::new().await;
    f.mock_container().await;
    f.mock_received(Some(SIZE as u64), None).await;
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .expect(0)
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    let UploadProgress::Complete { media } = a.upload_step(&s, cp).await.unwrap() else {
        panic!("the file had already arrived");
    };
    assert_eq!(media.id, "c1");
}

#[tokio::test]
async fn an_unreadable_upload_status_sends_the_whole_file() {
    let f = Fixture::new().await;
    f.mock_container().await;
    // Graph doesn't offer the field: the upload starts at 0.
    Mock::given(method("GET"))
        .and(path("/v24.0/c1"))
        .and(query_param("fields", "video_status"))
        .respond_with(graph_error(400, 100, None))
        .mount(&f.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .and(header("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .expect(1)
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    assert!(matches!(
        a.upload_step(&s, cp).await.unwrap(),
        UploadProgress::Complete { .. }
    ));
}

#[tokio::test]
async fn a_stale_container_is_replaced_before_uploading() {
    let f = Fixture::new().await;
    f.mock_container().await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let s = f.submission();
    let mut cp = a.begin_upload(&s).await.unwrap();
    cp.container_id = "old".into();
    cp.upload_url = format!("{}/ig-api-upload/v24.0/old", f.server.uri());
    *f.now.lock().unwrap() = t0() + chrono::Duration::hours(30);
    let UploadProgress::InProgress {
        checkpoint,
        fraction,
    } = a.upload_step(&s, cp).await.unwrap()
    else {
        panic!("expected a new container");
    };
    assert_eq!(checkpoint.container_id, "c1");
    assert_eq!(fraction, 0.0);
}

#[tokio::test]
async fn a_refused_token_is_renewed_once_then_reported() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media")))
        .and(header("authorization", "Bearer EAA-page-old"))
        .respond_with(graph_error(400, 190, Some(463)))
        .mount(&f.server)
        .await;
    f.mock_container().await;
    let tokens = Tokens::new(&["EAA-page-old", "EAA-page-one"]);
    let a = f.adapter(tokens.clone());
    let cp = a.begin_upload(&f.submission()).await.unwrap();
    assert_eq!(cp.container_id, "c1");
    assert_eq!(*tokens.invalidated.lock().unwrap(), vec!["EAA-page-old"]);

    let tokens = Tokens::new(&["EAA-page-old"]);
    let a = f.adapter(tokens);
    let err = a.begin_upload(&f.submission()).await.unwrap_err();
    assert!(
        matches!(err, AdapterError::Authentication { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn rate_limits_pause_instead_of_failing() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media")))
        .respond_with(graph_error(400, 4, None))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let err = a.begin_upload(&f.submission()).await.unwrap_err();
    assert!(matches!(err, AdapterError::RateLimited { .. }), "{err:?}");
}

#[tokio::test]
async fn the_daily_publishing_limit_is_respected_before_publishing() {
    let f = Fixture::new().await;
    f.mock_limit(100).await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media_publish")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "m1" })))
        .expect(0)
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let err = a.publish(&f.submission(), &f.media()).await.unwrap_err();
    assert!(
        matches!(err, AdapterError::RateLimited { ref message, .. } if message.contains("100")),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_publish_limit_answer_from_instagram_pauses_for_an_hour() {
    let f = Fixture::new().await;
    f.mock_limit(5).await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media_publish")))
        .respond_with(graph_error(400, 9, Some(2_207_042)))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let err = a.publish(&f.submission(), &f.media()).await.unwrap_err();
    assert!(
        matches!(err, AdapterError::RateLimited { retry_after, .. } if retry_after == Duration::from_secs(3600)),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_lost_publish_answer_is_reconciled_by_caption() {
    let f = Fixture::new().await;
    f.mock_limit(1).await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media_publish")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": "m1" }))
                .set_delay(Duration::from_secs(2)),
        )
        .mount(&f.server)
        .await;
    f.mock_status("PUBLISHED", None).await;
    Mock::given(method("GET"))
        .and(path(format!("/v24.0/{USER}/media")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "id": "m0", "caption": "Something older", "timestamp": "2026-10-29T10:00:00+0000" },
                { "id": "m1", "caption": f.caption(), "timestamp": "2026-10-30T18:01:05+0000",
                  "permalink": "https://www.instagram.com/reel/AbC123/" },
            ]
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let media = f.media();
    let err = a.publish(&f.submission(), &media).await.unwrap_err();
    assert!(matches!(err, AdapterError::ResponseLost { .. }), "{err:?}");

    let attempt = PublishAttempt::start(f.publication.id(), 1, None, t0());
    let Reconciliation::Found { post } = a.reconcile(&attempt, Some(&media)).await.unwrap() else {
        panic!("the reel exists");
    };
    assert_eq!(post.remote_id, "m1");
    assert_eq!(
        post.url.as_deref(),
        Some("https://www.instagram.com/reel/AbC123/")
    );
}

#[tokio::test]
async fn reconcile_reports_unpublished_and_unmatched_containers() {
    let f = Fixture::new().await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let attempt = PublishAttempt::start(f.publication.id(), 1, None, t0());

    f.mock_status("FINISHED", Some(1)).await;
    assert_eq!(
        a.reconcile(&attempt, Some(&f.media())).await.unwrap(),
        Reconciliation::NotFound
    );

    f.mock_status("PUBLISHED", None).await;
    Mock::given(method("GET"))
        .and(path(format!("/v24.0/{USER}/media")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "id": "m7", "caption": "Different words", "timestamp": "2026-10-30T18:01:05+0000" }]
        })))
        .mount(&f.server)
        .await;
    assert!(matches!(
        a.reconcile(&attempt, Some(&f.media())).await.unwrap(),
        Reconciliation::Unavailable { .. }
    ));

    // Without recorded media, the attempt's upload session names the container.
    let no_session = PublishAttempt::start(f.publication.id(), 1, None, t0());
    assert!(matches!(
        a.reconcile(&no_session, None).await.unwrap(),
        Reconciliation::Unavailable { .. }
    ));
}

#[tokio::test]
async fn processing_failures_are_reported() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v24.0/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status_code": "ERROR", "status": "Error: unsupported audio codec"
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.server)
        .await;
    f.mock_status("EXPIRED", None).await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let Processing::Failed { message } = a.check_processing(&f.media()).await.unwrap() else {
        panic!("expected a failure");
    };
    assert!(message.contains("unsupported audio codec"));
    let Processing::Failed { message } = a.check_processing(&f.media()).await.unwrap() else {
        panic!("expected a failure");
    };
    assert!(message.contains("expired"));
}

#[tokio::test]
async fn validation_follows_reels_rules() {
    let mut f = Fixture::new().await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    // Landscape is fine for Reels.
    f.props.width = 1920;
    f.props.height = 1080;
    assert!(a.validate(&f.submission()).await.unwrap().is_empty());

    f.props.duration_ms = 16 * 60_000;
    let issues = a.validate(&f.submission()).await.unwrap();
    assert!(matches!(issues[..], [ValidationIssue::TooLong { .. }]));

    f.props.duration_ms = 20_000;
    let webm = f._dir.path().join("reel.webm");
    std::fs::copy(&f.file, &webm).unwrap();
    f.file = webm;
    let issues = a.validate(&f.submission()).await.unwrap();
    assert!(matches!(
        &issues[..],
        [ValidationIssue::UnsupportedFormat { extension }] if extension == "webm"
    ));
}

#[tokio::test]
async fn capabilities_ask_for_a_reconnect_when_the_token_is_gone() {
    let f = Fixture::new().await;
    let a = f.adapter(Tokens::new(&[]));
    let c = a.capabilities(&f.account).await.unwrap();
    assert!(!c.can_publish_automatically());
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let c = a.capabilities(&f.account).await.unwrap();
    assert!(c.can_publish_automatically());
    assert!(c.notes.iter().any(|n| n.contains("100 posts")));
}

#[tokio::test]
async fn a_revoked_page_token_or_missing_permission_asks_for_a_reconnect() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media")))
        .respond_with(graph_error(400, 190, Some(460)))
        .mount(&f.server)
        .await;
    // The source hands out the same saved token again after it is refused.
    let tokens = Tokens::new(&["EAA-page-one", "EAA-page-one"]);
    let a = f.adapter(tokens.clone());
    let AdapterError::Authentication { message } =
        a.begin_upload(&f.submission()).await.unwrap_err()
    else {
        panic!("a revoked token needs a reconnect");
    };
    assert!(message.contains("password"), "{message}");
    assert_eq!(*tokens.invalidated.lock().unwrap(), vec!["EAA-page-one"]);

    let f = Fixture::new().await;
    f.mock_limit(0).await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{USER}/media_publish")))
        .respond_with(graph_error(403, 200, None))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["EAA-page-one"]));
    let AdapterError::Authentication { message } =
        a.publish(&f.submission(), &f.media()).await.unwrap_err()
    else {
        panic!("a missing permission needs a reconnect");
    };
    assert!(message.contains("instagram_content_publish"), "{message}");
}

#[tokio::test]
async fn pages_with_their_instagram_accounts_are_listed() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v24.0/me/accounts"))
        .and(query_param(
            "fields",
            "name,access_token,instagram_business_account{id,username}",
        ))
        .and(header("authorization", "Bearer EAA-user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "id": "p1", "name": "Bakery", "access_token": "EAA-page-one",
                  "instagram_business_account": { "id": USER, "username": "kitchen" } },
                { "id": "p2", "name": "Side project", "access_token": "EAA-page-two" },
            ],
            "paging": { "cursors": { "before": "a", "after": "b" } }
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&[]));
    let pages = a.pages("EAA-user").await.unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].access_token, "EAA-page-one");
    let ig = pages[0].instagram_business_account.as_ref().unwrap();
    assert_eq!((ig.id.as_str(), ig.username.as_str()), (USER, "kitchen"));
    assert!(pages[1].instagram_business_account.is_none());
    assert!(!format!("{pages:?}").contains("EAA-page"));
}

#[tokio::test]
async fn a_token_is_checked_with_debug_token_or_its_permissions() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v24.0/debug_token"))
        .and(query_param("input_token", "EAA-long"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "app_id": "1", "type": "USER", "is_valid": true,
                      "expires_at": 1_798_000_000, "data_access_expires_at": 1_798_000_000,
                      "scopes": ["instagram_basic", "pages_show_list"] }
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&[]));
    let check = a.check_token("EAA-long").await.unwrap();
    assert!(check.expiry_known);
    assert_eq!(check.expires_at.unwrap().timestamp(), 1_798_000_000);
    assert_eq!(check.scopes, vec!["instagram_basic", "pages_show_list"]);

    // debug_token refused (it would need the app's token): /me/permissions instead.
    Mock::given(method("GET"))
        .and(path("/v24.0/debug_token"))
        .and(query_param("input_token", "EAA-other"))
        .respond_with(graph_error(400, 100, None))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v24.0/me/permissions"))
        .and(header("authorization", "Bearer EAA-other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "permission": "instagram_basic", "status": "granted" },
                { "permission": "instagram_content_publish", "status": "declined" },
            ]
        })))
        .mount(&f.server)
        .await;
    let check = a.check_token("EAA-other").await.unwrap();
    assert!(!check.expiry_known);
    assert_eq!(check.scopes, vec!["instagram_basic"]);
}

#[tokio::test]
async fn an_instagram_login_token_still_identifies_its_account() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/ig/v24.0/me"))
        .and(header("authorization", "Bearer IGAA-pasted"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "app-scoped", "user_id": USER, "username": "kitchen", "account_type": "MEDIA_CREATOR"
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&[]));
    let profile = a.profile("IGAA-pasted").await.unwrap();
    assert_eq!(profile.publishing_id(), USER);
    assert_eq!(profile.username, "kitchen");
    assert!(profile.can_publish());
    assert!(!format!("{profile:?}").contains(USER));
}
