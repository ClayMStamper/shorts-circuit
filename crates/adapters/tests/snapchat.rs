//! The Snapchat adapter and OAuth calls against a local fake of Snap's Business API and
//! accounts endpoints (wiremock). Nothing here calls Snap.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use shorts_adapters::snapchat::{
    SnapAdapter, SnapAuth, SnapAuthError, SnapClient, SnapConfig, SnapEndpoints, SnapMedia,
    SnapOAuth, SnapSession, SnapUpload,
};
use shorts_adapters::{
    Adapter, AdapterError, Processing, Reconciliation, Submission, UploadProgress,
};
use shorts_domain::{
    AccountId, AssetId, Capabilities, ConnectedAccount, Destination, MediaProperties, Platform,
    Publication, PublishAccess, PublishAttempt, RevisionContent, ValidationIssue, VariantId,
};
use tempfile::TempDir;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SIZE: usize = 100 * 1024;
const PART: usize = 64 * 1024;
const PROFILE: &str = "pp-123";

/// `body_string_contains` never matches a body that isn't UTF-8, and parts carry ciphertext.
fn body_has(needle: &'static [u8]) -> impl Fn(&Request) -> bool {
    move |req: &Request| req.body.windows(needle.len()).any(|w| w == needle)
}

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
impl SnapAuth for Tokens {
    async fn session(&self, _account: AccountId) -> Result<SnapSession, AdapterError> {
        let mut current = self.current.lock().unwrap();
        if current.is_none() {
            *current = self.queue.lock().unwrap().pop_front();
        }
        current
            .clone()
            .map(|access_token| SnapSession {
                access_token,
                profile_id: PROFILE.into(),
            })
            .ok_or_else(|| AdapterError::Authentication {
                message: "signed out".into(),
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
        Self::with_caption("Patch notes are live\nPlaytest now").await
    }

    async fn with_caption(caption: &str) -> Self {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("spot.mp4");
        let bytes: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, bytes).unwrap();
        let mut account =
            ConnectedAccount::manual(Platform::SnapchatSpotlight, "Timeless Way", t0());
        account.capabilities = Capabilities {
            publish_access: PublishAccess::Automatic,
            ..Capabilities::manual_only(t0())
        };
        let content = RevisionContent::new(
            VariantId::new(),
            1,
            caption,
            &["gamedev".into()],
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

    fn adapter(&self, tokens: Arc<dyn SnapAuth>) -> SnapAdapter {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let now = self.now.clone();
        SnapAdapter::new(
            http,
            SnapConfig {
                api_base: self.server.uri().parse().unwrap(),
                locale: "en_US".into(),
                part_bytes: PART,
                part_timeout: Duration::from_millis(500),
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

    fn media(&self) -> SnapMedia {
        SnapMedia {
            id: "media-1".into(),
            account_id: self.account.id,
            created_at: t0(),
        }
    }

    async fn mock_create(&self, media_id: &str) {
        let media_id = media_id.to_owned();
        Mock::given(method("POST"))
            .and(path(format!("/v1/public_profiles/{PROFILE}/media")))
            .and(header("authorization", "Bearer snap-one"))
            .respond_with(move |req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["type"], "VIDEO");
                let key = BASE64.decode(body["key"].as_str().unwrap()).unwrap();
                let iv = BASE64.decode(body["iv"].as_str().unwrap()).unwrap();
                assert_eq!((key.len(), iv.len()), (32, 16));
                ResponseTemplate::new(200).set_body_json(json!({
                    "request_id": "r1",
                    "request_status": "SUCCESS",
                    "media_id": media_id,
                    "add_path": format!("/us/v1/public_profiles/{PROFILE}/media/{media_id}/multipart-upload-v2?action=ADD"),
                    "finalize_path": format!("/us/v1/public_profiles/{PROFILE}/media/{media_id}/multipart-upload-v2?action=FINALIZE"),
                }))
            })
            .mount(&self.server)
            .await;
    }

    async fn mock_parts(&self, media_id: &str) {
        let upload =
            format!("/us/v1/public_profiles/{PROFILE}/media/{media_id}/multipart-upload-v2");
        Mock::given(method("POST"))
            .and(path(upload.clone()))
            .and(query_param("action", "ADD"))
            .and(body_has(b"name=\"action\"\r\n\r\nADD"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "request_status": "SUCCESS" })),
            )
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path(upload))
            .and(query_param("action", "FINALIZE"))
            .and(body_has(b"FINALIZE"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "request_status": "SUCCESS" })),
            )
            .expect(1)
            .mount(&self.server)
            .await;
    }

    async fn part_numbers(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.query() == Some("action=ADD"))
            .map(|r| {
                let body = String::from_utf8_lossy(&r.body);
                let after = body.split("name=\"part_number\"\r\n\r\n").nth(1).unwrap();
                after.split("\r\n").next().unwrap().to_owned()
            })
            .collect()
    }
}

#[tokio::test]
async fn a_video_is_encrypted_uploaded_in_parts_and_submitted_to_spotlight() {
    let f = Fixture::new().await;
    f.mock_create("media-1").await;
    f.mock_parts("media-1").await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/public_profiles/{PROFILE}/spotlights")))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["media_id"], "media-1");
            assert_eq!(body["locale"], "en_US");
            assert_eq!(
                body["description"],
                "Patch notes are live Playtest now  #gamedev"
            );
            ResponseTemplate::new(200).set_body_json(json!({
                "request_status": "SUCCESS", "spotlight_id": "spot-9"
            }))
        })
        .expect(1)
        .mount(&f.server)
        .await;

    let a = f.adapter(Tokens::new(&["snap-one"]));
    let s = f.submission();
    assert!(a.validate(&s).await.unwrap().is_empty());
    let cp = a.begin_upload(&s).await.unwrap();
    // 100 KiB encrypts to 100 KiB + 16 bytes: two 64 KiB parts.
    assert_eq!((cp.parts, cp.sent), (2, 0));
    let UploadProgress::InProgress {
        checkpoint,
        fraction,
    } = a.upload_step(&s, cp).await.unwrap()
    else {
        panic!("two parts to send");
    };
    assert_eq!(fraction, 0.5);
    let UploadProgress::Complete { media } = a.upload_step(&s, checkpoint).await.unwrap() else {
        panic!("the last part finishes the upload");
    };
    assert_eq!(media.id, "media-1");
    assert_eq!(f.part_numbers().await, vec!["1", "2"]);
    assert_eq!(a.check_processing(&media).await.unwrap(), Processing::Ready);
    let post = a.publish(&s, &media).await.unwrap();
    assert_eq!(post.remote_id, "spot-9");
    assert!(post.url.is_none());
    assert!(post.note.unwrap().contains("reviews"));
}

#[tokio::test]
async fn a_restart_reencrypts_and_continues_with_the_next_part() {
    let f = Fixture::new().await;
    f.mock_create("media-1").await;
    f.mock_parts("media-1").await;
    let a = f.adapter(Tokens::new(&["snap-one"]));
    let s = f.submission();
    let cp = a.begin_upload(&s).await.unwrap();
    let UploadProgress::InProgress { checkpoint, .. } = a.upload_step(&s, cp).await.unwrap() else {
        panic!("one part left");
    };
    let stored: SnapUpload =
        serde_json::from_value(serde_json::to_value(&checkpoint).unwrap()).unwrap();
    assert_eq!(stored.sent, 1);
    let fresh = f.adapter(Tokens::new(&["snap-one"]));
    assert!(matches!(
        fresh.upload_step(&s, stored).await.unwrap(),
        UploadProgress::Complete { .. }
    ));
    assert_eq!(
        f.part_numbers().await,
        vec!["1", "2"],
        "part 1 isn't sent twice"
    );
    assert!(!format!("{checkpoint:?}").contains(&checkpoint.key));
}

#[tokio::test]
async fn an_upload_older_than_a_day_starts_over() {
    let f = Fixture::new().await;
    f.mock_create("media-2").await;
    let a = f.adapter(Tokens::new(&["snap-one"]));
    let s = f.submission();
    let mut cp = a.begin_upload(&s).await.unwrap();
    cp.media_id = "old".into();
    *f.now.lock().unwrap() = t0() + chrono::Duration::hours(25);
    let UploadProgress::InProgress { checkpoint, .. } = a.upload_step(&s, cp).await.unwrap() else {
        panic!("expected a new upload");
    };
    assert_eq!(checkpoint.media_id, "media-2");
    assert_eq!(checkpoint.sent, 0);
}

#[tokio::test]
async fn refused_access_explains_the_allowlist() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/public_profiles/{PROFILE}/media")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "request_status": "ERROR", "error_code": "E1002", "display_message": "Unauthorized client"
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["snap-one"]));
    let AdapterError::Authentication { message } =
        a.begin_upload(&f.submission()).await.unwrap_err()
    else {
        panic!("expected a reconnect");
    };
    assert!(message.contains("allowlisted"));
}

#[tokio::test]
async fn an_expired_token_is_refreshed_once() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/public_profiles/{PROFILE}/media")))
        .and(header("authorization", "Bearer snap-old"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&f.server)
        .await;
    f.mock_create("media-1").await;
    let tokens = Tokens::new(&["snap-old", "snap-one"]);
    let a = f.adapter(tokens.clone());
    let cp = a.begin_upload(&f.submission()).await.unwrap();
    assert_eq!(cp.media_id, "media-1");
    assert_eq!(*tokens.invalidated.lock().unwrap(), vec!["snap-old"]);
}

#[tokio::test]
async fn expired_media_and_rate_limits_are_reported() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/public_profiles/{PROFILE}/spotlights")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "request_status": "ERROR", "error_code": "MEDIA_EXPIRED", "debug_message": "media expired"
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/public_profiles/{PROFILE}/spotlights")))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "30"))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["snap-one"]));
    let AdapterError::Invalid { issues } =
        a.publish(&f.submission(), &f.media()).await.unwrap_err()
    else {
        panic!("expected a validation problem");
    };
    assert!(issues[0].contains("24 hours"));
    assert!(matches!(
        a.publish(&f.submission(), &f.media()).await.unwrap_err(),
        AdapterError::RateLimited { retry_after, .. } if retry_after == Duration::from_secs(30)
    ));
}

#[tokio::test]
async fn reconcile_finds_the_post_made_from_the_media_or_defers_to_the_user() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/public_profiles/{PROFILE}/spotlights")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_status": "SUCCESS",
            "spotlights": [
                { "spotlight": { "id": "spot-1", "media_id": "media-0", "status": "LIVE" } },
                { "spotlight": { "id": "spot-2", "media_id": "media-1", "status": "SUBMITTED" } }
            ]
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&["snap-one"]));
    let attempt = PublishAttempt::start(f.publication.id(), 1, None, t0());
    let Reconciliation::Found { post } = a.reconcile(&attempt, Some(&f.media())).await.unwrap()
    else {
        panic!("the post exists");
    };
    assert_eq!(post.remote_id, "spot-2");
    let other = SnapMedia {
        id: "media-7".into(),
        ..f.media()
    };
    assert!(matches!(
        a.reconcile(&attempt, Some(&other)).await.unwrap(),
        Reconciliation::Unavailable { .. }
    ));
}

#[tokio::test]
async fn the_signed_in_profile_is_read() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/public_profiles/my_profile"))
        .and(header("authorization", "Bearer fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_status": "SUCCESS",
            "public_profile": {
                "id": PROFILE, "display_name": "Timeless Way", "snap_user_name": "timelessway",
                "profile_type": "PUBLIC"
            }
        })))
        .mount(&f.server)
        .await;
    let a = f.adapter(Tokens::new(&[]));
    let p = a.my_profile("fresh").await.unwrap();
    assert_eq!(
        (p.id.as_str(), p.display_name.as_str(), p.username.as_str()),
        (PROFILE, "Timeless Way", "timelessway")
    );
}

#[tokio::test]
async fn validation_follows_spotlight_rules() {
    let mut f = Fixture::new().await;
    let a = f.adapter(Tokens::new(&["snap-one"]));
    f.props.duration_ms = 5_000;
    assert!(matches!(
        a.validate(&f.submission()).await.unwrap()[..],
        [ValidationIssue::TooShort { .. }]
    ));
    f.props.duration_ms = 20_000;
    f.props.width = 480;
    f.props.height = 854;
    assert!(matches!(
        a.validate(&f.submission()).await.unwrap()[..],
        [ValidationIssue::ResolutionTooLow { .. }]
    ));
    let long = Fixture::with_caption(&"x".repeat(170)).await;
    let a = long.adapter(Tokens::new(&["snap-one"]));
    assert!(matches!(
        a.validate(&long.submission()).await.unwrap()[..],
        [ValidationIssue::CaptionTooLong { .. }]
    ));
}

#[tokio::test]
async fn oauth_exchanges_and_refreshes_with_the_client_secret() {
    let server = MockServer::start().await;
    let oauth = SnapOAuth::new(
        reqwest::Client::new(),
        SnapEndpoints {
            authorize: format!("{}/authorize", server.uri()).parse().unwrap(),
            token: format!("{}/token", server.uri()).parse().unwrap(),
        },
    );
    let client = SnapClient::new("client-id-123", "client-secret-456").unwrap();
    let url = oauth.authorization_url(&client, "http://127.0.0.1:47615/snapchat/callback", "st");
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["scope"], "snapchat-profile-api");
    assert_eq!(q["response_type"], "code");
    assert!(!url.as_str().contains("client-secret"));

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_secret=client-secret-456"))
        .and(body_string_contains("code=abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at1", "refresh_token": "rt1", "expires_in": 3600, "token_type": "Bearer"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=revoked"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant", "error_description": "Invalid refresh token"
        })))
        .mount(&server)
        .await;
    let tokens = oauth
        .exchange_code(&client, "abc", "http://127.0.0.1:47615/snapchat/callback")
        .await
        .unwrap();
    assert_eq!(tokens.access_token, "at1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("rt1"));
    assert!(!format!("{tokens:?}").contains("at1"));
    assert!(matches!(
        oauth.refresh(&client, "revoked").await.unwrap_err(),
        SnapAuthError::Reauthorize { .. }
    ));
}
