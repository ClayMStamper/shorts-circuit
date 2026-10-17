//! The Instagram adapter against a local fake of the Instagram Graph API and its upload host
//! (wiremock). Nothing here calls Meta.

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
            .and(header("authorization", "Bearer IGAA-one"))
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
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .and(header("authorization", "OAuth IGAA-one"))
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

    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
    let cp = a.begin_upload(&f.submission()).await.unwrap();
    assert_eq!(
        cp.upload_url,
        format!("{}/rupload/ig-api-upload/v24.0/c9", f.server.uri())
    );
}

#[tokio::test]
async fn a_broken_upload_is_sent_again_to_the_same_container() {
    let f = Fixture::new().await;
    f.mock_container().await;
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": { "message": "try again", "code": 2, "is_transient": true }
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ig-api-upload/v24.0/c1"))
        .and(header("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    let err = a.upload_step(&s, cp.clone()).await.unwrap_err();
    assert!(matches!(err, AdapterError::Transient { .. }), "{err:?}");
    // A restart resumes from the stored checkpoint.
    let stored: InstagramUpload =
        serde_json::from_value(serde_json::to_value(&cp).unwrap()).unwrap();
    assert!(matches!(
        a.upload_step(&s, stored).await.unwrap(),
        UploadProgress::Complete { .. }
    ));
}

#[tokio::test]
async fn a_stale_container_is_replaced_before_uploading() {
    let f = Fixture::new().await;
    f.mock_container().await;
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
        .and(header("authorization", "Bearer IGAA-old"))
        .respond_with(graph_error(400, 190, Some(463)))
        .mount(&f.server)
        .await;
    f.mock_container().await;
    let tokens = Tokens::new(&["IGAA-old", "IGAA-one"]);
    let a = f.adapter(tokens.clone());
    let cp = a.begin_upload(&f.submission()).await.unwrap();
    assert_eq!(cp.container_id, "c1");
    assert_eq!(*tokens.invalidated.lock().unwrap(), vec!["IGAA-old"]);

    let tokens = Tokens::new(&["IGAA-old"]);
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
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
    let a = f.adapter(Tokens::new(&["IGAA-one"]));
    let c = a.capabilities(&f.account).await.unwrap();
    assert!(c.can_publish_automatically());
    assert!(c.notes.iter().any(|n| n.contains("100 posts")));
}

#[tokio::test]
async fn a_pasted_token_identifies_the_account_and_can_be_renewed() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v24.0/me"))
        .and(header("authorization", "Bearer IGAA-pasted"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "app-scoped", "user_id": USER, "username": "kitchen", "account_type": "MEDIA_CREATOR"
        })))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/refresh_access_token"))
        .and(query_param("grant_type", "ig_refresh_token"))
        .and(query_param("access_token", "IGAA-pasted"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "IGAA-renewed", "token_type": "bearer", "expires_in": 5_183_944
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&[]));
    let profile = a.profile("IGAA-pasted").await.unwrap();
    assert_eq!(profile.publishing_id(), USER);
    assert_eq!(profile.username, "kitchen");
    assert!(profile.can_publish());
    assert!(!format!("{profile:?}").contains(USER));
    let renewed = a.refresh_token("IGAA-pasted").await.unwrap();
    assert_eq!(renewed.access_token, "IGAA-renewed");
    assert_eq!(renewed.expires_in, 5_183_944);
    assert!(!format!("{renewed:?}").contains("renewed"));
}
