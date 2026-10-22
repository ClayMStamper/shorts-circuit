//! The YouTube adapter against a local fake of the Data API (wiremock). Nothing here calls
//! Google.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;
use shorts_adapters::youtube::{
    AccessTokens, BUCKET_UNITS, BUCKET_UPLOADS, MemoryQuota, QuotaLedger, QuotaLimits,
    YouTubeAdapter, YouTubeConfig, YouTubeMedia, YouTubeUpload, marker, quota_day, quota_resets_at,
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
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CHUNK: u64 = 256 * 1024;
const SIZE: u64 = 600 * 1024;

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
impl AccessTokens for Tokens {
    async fn access_token(&self, _account: AccountId) -> Result<String, AdapterError> {
        let mut current = self.current.lock().unwrap();
        if current.is_none() {
            *current = self.queue.lock().unwrap().pop_front();
        }
        current.clone().ok_or_else(|| AdapterError::Authentication {
            message: "the refresh token was revoked".into(),
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
    quota: Arc<MemoryQuota>,
}

impl Fixture {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("short.mp4");
        let bytes: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, bytes).unwrap();
        let mut account = ConnectedAccount::manual(Platform::YoutubeShorts, "Test channel", t0());
        account.capabilities = Capabilities {
            publish_access: PublishAccess::Automatic,
            private_only: true,
            ..Capabilities::manual_only(t0())
        };
        let content = RevisionContent::new(
            VariantId::new(),
            1,
            "Crispy edges every time\nFull recipe in bio",
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
            quota: Arc::new(MemoryQuota::default()),
        }
    }

    fn adapter(&self, tokens: Arc<dyn AccessTokens>) -> YouTubeAdapter {
        self.adapter_with(tokens, QuotaLimits::default())
    }

    fn adapter_with(&self, tokens: Arc<dyn AccessTokens>, limits: QuotaLimits) -> YouTubeAdapter {
        self.build(tokens, limits, CHUNK)
    }

    fn build(
        &self,
        tokens: Arc<dyn AccessTokens>,
        limits: QuotaLimits,
        chunk_bytes: u64,
    ) -> YouTubeAdapter {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        YouTubeAdapter::new(
            http,
            YouTubeConfig {
                api_base: self.server.uri().parse().unwrap(),
                chunk_bytes,
                limits,
                poll_every: Duration::from_millis(1),
            },
            tokens,
            self.quota.clone(),
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

    fn session_url(&self) -> String {
        format!("{}/upload/session?upload_id=abc", self.server.uri())
    }

    fn marker(&self) -> String {
        marker(self.publication.id(), self.publication.revision().number)
    }

    async fn mock_initiation(&self, expect: u64) {
        Mock::given(method("POST"))
            .and(path("/upload/youtube/v3/videos"))
            .and(query_param("uploadType", "resumable"))
            .and(header("X-Upload-Content-Length", SIZE.to_string().as_str()))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).insert_header("Location", self.session_url()))
            .expect(expect)
            .mount(&self.server)
            .await;
    }

    async fn used(&self, bucket: &str) -> u32 {
        self.quota.used(bucket, quota_day(t0())).await.unwrap()
    }
}

fn chunk(from: u64, to: u64) -> String {
    format!("bytes {from}-{to}/{SIZE}")
}

fn incomplete(last: u64) -> ResponseTemplate {
    ResponseTemplate::new(308).insert_header("Range", format!("bytes=0-{last}"))
}

fn video_json(id: &str, upload: &str, processing: &str, privacy: &str) -> serde_json::Value {
    json!({
        "items": [{
            "id": id,
            "status": { "uploadStatus": upload, "privacyStatus": privacy },
            "processingDetails": {
                "processingStatus": processing,
                "processingProgress": { "partsTotal": "4", "partsProcessed": "1" }
            }
        }]
    })
}

async fn upload_all(
    adapter: &YouTubeAdapter,
    sub: &Submission<'_>,
    mut cp: YouTubeUpload,
) -> Result<YouTubeMedia, AdapterError> {
    for _ in 0..10 {
        match adapter.upload_step(sub, cp).await? {
            UploadProgress::InProgress { checkpoint, .. } => cp = checkpoint,
            UploadProgress::Complete { media } => return Ok(media),
        }
    }
    panic!("upload did not finish");
}

#[tokio::test]
async fn uploads_in_chunks_then_processes_and_publishes_as_private() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/upload/youtube/v3/videos"))
        .and(body_partial_json(json!({
            "snippet": { "title": "Crispy edges every time", "categoryId": "22" },
            "status": { "privacyStatus": "private", "selfDeclaredMadeForKids": false }
        })))
        .respond_with(ResponseTemplate::new(200).insert_header("Location", f.session_url()))
        .expect(1)
        .mount(&f.server)
        .await;
    for (from, to, response) in [
        (0, CHUNK - 1, incomplete(CHUNK - 1)),
        (CHUNK, 2 * CHUNK - 1, incomplete(2 * CHUNK - 1)),
        (
            2 * CHUNK,
            SIZE - 1,
            ResponseTemplate::new(201).set_body_json(json!({ "id": "vid1" })),
        ),
    ] {
        Mock::given(method("PUT"))
            .and(path("/upload/session"))
            .and(header("Content-Range", chunk(from, to).as_str()))
            .respond_with(response)
            .expect(1)
            .mount(&f.server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .and(query_param("part", "status,processingDetails"))
        .respond_with(ResponseTemplate::new(200).set_body_json(video_json(
            "vid1",
            "uploaded",
            "processing",
            "private",
        )))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(video_json(
            "vid1",
            "processed",
            "succeeded",
            "private",
        )))
        .mount(&f.server)
        .await;

    let adapter = f.adapter(Tokens::new(&["tok"]));
    let sub = f.submission();
    assert!(adapter.validate(&sub).await.unwrap().is_empty());
    let cp = adapter.begin_upload(&sub).await.unwrap();
    assert_eq!((cp.offset, cp.total), (0, SIZE));
    assert_eq!(cp.privacy, "private");

    let mut fractions = Vec::new();
    let mut cp = cp;
    let media = loop {
        match adapter.upload_step(&sub, cp).await.unwrap() {
            UploadProgress::InProgress {
                checkpoint,
                fraction,
            } => {
                fractions.push(fraction);
                cp = checkpoint;
            }
            UploadProgress::Complete { media } => break media,
        }
    };
    assert_eq!(media.id, "vid1");
    assert_eq!(fractions.len(), 2);
    assert!(fractions[0] < fractions[1] && fractions[1] < 1.0);

    let first = adapter.check_processing(&media).await.unwrap();
    assert!(matches!(
        first,
        Processing::Pending { detail: Some(ref d), .. } if d.contains("25%")
    ));
    assert_eq!(
        adapter.check_processing(&media).await.unwrap(),
        Processing::Ready
    );

    let post = adapter.publish(&sub, &media).await.unwrap();
    assert_eq!(
        post.url.as_deref(),
        Some("https://www.youtube.com/shorts/vid1")
    );
    assert_eq!(post.post.privacy_status, "private");
    assert!(post.note.unwrap().contains("private"));

    // The marker tag went up with the video, so it can be found after a lost response.
    let initiation = f
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.method.as_str() == "POST")
        .unwrap();
    let body: serde_json::Value = initiation.body_json().unwrap();
    assert_eq!(body["snippet"]["tags"][0], f.marker());
    assert_eq!(f.used(BUCKET_UPLOADS).await, 1);
    assert_eq!(f.used(BUCKET_UNITS).await, 3);
}

#[tokio::test]
async fn a_restart_asks_the_session_and_resumes_mid_file() {
    let f = Fixture::new().await;
    f.mock_initiation(1).await;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .and(header("Content-Range", chunk(0, CHUNK - 1).as_str()))
        .respond_with(incomplete(CHUNK - 1))
        .expect(1)
        .mount(&f.server)
        .await;
    // YouTube got part of the next chunk before the process died.
    let got = 393_216;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .and(header("Content-Range", format!("bytes */{SIZE}").as_str()))
        .respond_with(incomplete(got - 1))
        .expect(1)
        .mount(&f.server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .and(header("Content-Range", chunk(got, SIZE - 1).as_str()))
        .respond_with(move |r: &Request| {
            // The resumed bytes are the file's bytes from the confirmed offset.
            let expected: Vec<u8> = (got..SIZE).map(|i| (i % 251) as u8).collect();
            assert_eq!(r.body, expected);
            ResponseTemplate::new(200).set_body_json(json!({ "id": "vid1" }))
        })
        .expect(1)
        .mount(&f.server)
        .await;

    let sub = f.submission();
    let first = f.adapter(Tokens::new(&["tok"]));
    let cp = first.begin_upload(&sub).await.unwrap();
    let UploadProgress::InProgress { checkpoint, .. } = first.upload_step(&sub, cp).await.unwrap()
    else {
        panic!("one chunk of three")
    };
    assert_eq!(checkpoint.offset, CHUNK);
    // The checkpoint as the scheduler stores it, read back by a new process.
    let stored: YouTubeUpload =
        serde_json::from_str(&serde_json::to_string(&checkpoint).unwrap()).unwrap();
    drop(first);

    let second = f.adapter(Tokens::new(&["tok"]));
    let media = upload_all(&second, &sub, stored).await.unwrap();
    assert_eq!(media.id, "vid1");
}

#[tokio::test]
async fn a_lost_final_response_is_read_back_from_the_session() {
    let f = Fixture::new().await;
    f.mock_initiation(1).await;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .and(header("Content-Range", chunk(0, SIZE - 1).as_str()))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({ "id": "vid1" }))
                .set_delay(Duration::from_secs(3)),
        )
        .expect(1)
        .mount(&f.server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .and(header("Content-Range", format!("bytes */{SIZE}").as_str()))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "vid1" })))
        .expect(1)
        .mount(&f.server)
        .await;

    let sub = f.submission();
    // One chunk for the whole file.
    let adapter = f.build(Tokens::new(&["tok"]), QuotaLimits::default(), SIZE);
    let cp = adapter.begin_upload(&sub).await.unwrap();
    let lost = adapter.upload_step(&sub, cp.clone()).await.unwrap_err();
    assert!(
        matches!(lost, AdapterError::ResponseLost { .. }),
        "{lost:?}"
    );
    // The retry asks the session instead of sending the file again.
    let UploadProgress::Complete { media } = adapter.upload_step(&sub, cp).await.unwrap() else {
        panic!("the session says it finished")
    };
    assert_eq!(media.id, "vid1");
}

#[tokio::test]
async fn an_expired_session_finds_the_video_by_its_marker_instead_of_uploading_again() {
    let f = Fixture::new().await;
    f.mock_initiation(0).await;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/channels"))
        .and(query_param("mine", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": "UCtest", "contentDetails": { "relatedPlaylists": { "uploads": "UUtest" } } }]
        })))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/playlistItems"))
        .and(query_param("playlistId", "UUtest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "contentDetails": { "videoId": "other" } },
                { "contentDetails": { "videoId": "vid1" } }
            ]
        })))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .and(query_param("id", "other,vid1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "id": "other", "snippet": { "tags": ["bread"] } },
                { "id": "vid1", "snippet": { "tags": [f.marker(), "bread"] } }
            ]
        })))
        .mount(&f.server)
        .await;

    let sub = f.submission();
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let stale = YouTubeUpload {
        account_id: f.account.id,
        upload_url: f.session_url(),
        offset: SIZE,
        total: SIZE,
        marker: f.marker(),
        privacy: "private".into(),
        started_at: t0(),
    };
    let UploadProgress::Complete { media } = adapter.upload_step(&sub, stale).await.unwrap() else {
        panic!("the earlier upload was found")
    };
    assert_eq!(media.id, "vid1");
}

#[tokio::test]
async fn an_expired_session_without_a_finished_video_starts_over() {
    let f = Fixture::new().await;
    f.mock_initiation(1).await;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/channels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": "UCtest", "contentDetails": { "relatedPlaylists": { "uploads": "UUtest" } } }]
        })))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/playlistItems"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
        .mount(&f.server)
        .await;

    let adapter = f.adapter(Tokens::new(&["tok"]));
    let stale = YouTubeUpload {
        account_id: f.account.id,
        upload_url: f.session_url(),
        offset: CHUNK,
        total: SIZE,
        marker: f.marker(),
        privacy: "private".into(),
        started_at: t0(),
    };
    let UploadProgress::InProgress { checkpoint, .. } =
        adapter.upload_step(&f.submission(), stale).await.unwrap()
    else {
        panic!("a new session")
    };
    assert_eq!(checkpoint.offset, 0);
}

#[tokio::test]
async fn quota_exceeded_pauses_until_midnight_pacific_and_stops_further_calls() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/upload/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {
                "code": 403,
                "message": "The request cannot be completed because you have exceeded your quota.",
                "errors": [{ "reason": "quotaExceeded", "domain": "youtube.quota" }]
            }
        })))
        .expect(1)
        .mount(&f.server)
        .await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let sub = f.submission();
    let err = adapter.begin_upload(&sub).await.unwrap_err();
    let AdapterError::RateLimited { retry_after, .. } = err else {
        panic!("{err:?}")
    };
    let reset = (quota_resets_at(t0()) - t0()).to_std().unwrap();
    assert!(retry_after >= reset && retry_after <= reset + Duration::from_secs(300));
    assert_eq!(f.used(BUCKET_UPLOADS).await, 100);

    // Counted as used up: the next upload waits without calling YouTube.
    let again = adapter.begin_upload(&sub).await.unwrap_err();
    assert!(matches!(again, AdapterError::RateLimited { .. }));
}

#[tokio::test]
async fn uploads_stop_before_the_daily_quota_runs_out() {
    let f = Fixture::new().await;
    f.mock_initiation(1).await;
    let limits = QuotaLimits {
        uploads_per_day: 1,
        units_per_day: 10_000,
    };
    let adapter = f.adapter_with(Tokens::new(&["tok"]), limits);
    let sub = f.submission();
    adapter.begin_upload(&sub).await.unwrap();
    assert!(matches!(
        adapter.begin_upload(&sub).await,
        Err(AdapterError::RateLimited { .. })
    ));
    // Nor does an upload start when there is too little left to watch it process.
    f.quota
        .spend(BUCKET_UNITS, quota_day(t0()), 9_990)
        .await
        .unwrap();
    let roomy = f.adapter_with(
        Tokens::new(&["tok"]),
        QuotaLimits {
            uploads_per_day: 100,
            units_per_day: 10_000,
        },
    );
    assert!(matches!(
        roomy.begin_upload(&sub).await,
        Err(AdapterError::RateLimited { .. })
    ));
}

#[tokio::test]
async fn rate_limit_exceeded_pauses_briefly() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": { "code": 403, "message": "slow down", "errors": [{ "reason": "rateLimitExceeded" }] }
        })))
        .mount(&f.server)
        .await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let media = YouTubeMedia {
        id: "vid1".into(),
        account_id: f.account.id,
        privacy: "private".into(),
        uploaded_at: t0(),
    };
    let err = adapter.check_processing(&media).await.unwrap_err();
    assert_eq!(
        err,
        AdapterError::RateLimited {
            message: "YouTube asked to slow down: slow down".into(),
            retry_after: Duration::from_secs(60),
        }
    );
}

#[tokio::test]
async fn an_expired_access_token_is_replaced_once() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .and(header("authorization", "Bearer stale"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "code": 401, "message": "Invalid Credentials", "errors": [{ "reason": "authError" }] }
        })))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .and(header("authorization", "Bearer fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(video_json(
            "vid1",
            "processed",
            "succeeded",
            "private",
        )))
        .mount(&f.server)
        .await;
    let tokens = Tokens::new(&["stale", "fresh"]);
    let adapter = f.adapter(tokens.clone());
    let media = YouTubeMedia {
        id: "vid1".into(),
        account_id: f.account.id,
        privacy: "private".into(),
        uploaded_at: t0(),
    };
    assert_eq!(
        adapter.check_processing(&media).await.unwrap(),
        Processing::Ready
    );
    assert_eq!(
        *tokens.invalidated.lock().unwrap(),
        vec!["stale".to_owned()]
    );

    // When no working token can be had, the channel needs reconnecting.
    let revoked = f.adapter(Tokens::new(&["stale"]));
    assert!(matches!(
        revoked.check_processing(&media).await,
        Err(AdapterError::Authentication { .. })
    ));
}

#[tokio::test]
async fn processing_failures_are_reported() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .and(query_param("id", "bad"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "bad",
                "status": { "uploadStatus": "uploaded", "privacyStatus": "private" },
                "processingDetails": { "processingStatus": "failed", "processingFailureReason": "transcodeFailed" }
            }]
        })))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .and(query_param("id", "dup"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "dup",
                "status": { "uploadStatus": "rejected", "rejectionReason": "duplicate", "privacyStatus": "private" }
            }]
        })))
        .mount(&f.server)
        .await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let media = |id: &str| YouTubeMedia {
        id: id.into(),
        account_id: f.account.id,
        privacy: "private".into(),
        uploaded_at: t0(),
    };
    let Processing::Failed { message } = adapter.check_processing(&media("bad")).await.unwrap()
    else {
        panic!()
    };
    assert!(message.contains("transcodeFailed"));
    let Processing::Failed { message } = adapter.check_processing(&media("dup")).await.unwrap()
    else {
        panic!()
    };
    assert!(message.contains("duplicate"));
}

#[tokio::test]
async fn a_lost_publish_response_is_reconciled_by_video_id() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(video_json("vid1", "processed", "succeeded", "public"))
                .set_delay(Duration::from_secs(3)),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(video_json(
            "vid1",
            "processed",
            "succeeded",
            "public",
        )))
        .mount(&f.server)
        .await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let media = YouTubeMedia {
        id: "vid1".into(),
        account_id: f.account.id,
        privacy: "public".into(),
        uploaded_at: t0(),
    };
    let sub = f.submission();
    assert!(matches!(
        adapter.publish(&sub, &media).await,
        Err(AdapterError::ResponseLost { .. })
    ));
    let attempt = PublishAttempt::start(f.publication.id(), 1, None, t0());
    let Reconciliation::Found { post } = adapter.reconcile(&attempt, Some(&media)).await.unwrap()
    else {
        panic!("the video is there")
    };
    assert_eq!(post.remote_id, "vid1");
    assert_eq!(post.note, None, "public as requested");
}

#[tokio::test]
async fn reconcile_without_a_video_id_asks_the_session_then_gives_up_honestly() {
    let f = Fixture::new().await;
    Mock::given(method("PUT"))
        .and(path("/upload/session"))
        .respond_with(incomplete(CHUNK - 1))
        .mount(&f.server)
        .await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let mut attempt = PublishAttempt::start(f.publication.id(), 1, None, t0());
    let none = adapter.reconcile(&attempt, None).await.unwrap();
    assert!(matches!(none, Reconciliation::Unavailable { .. }));
    attempt.upload_session = Some(
        serde_json::to_value(YouTubeUpload {
            account_id: f.account.id,
            upload_url: f.session_url(),
            offset: CHUNK,
            total: SIZE,
            marker: f.marker(),
            privacy: "private".into(),
            started_at: t0(),
        })
        .unwrap(),
    );
    // An unfinished session means there is no video yet, but the user decides.
    assert!(matches!(
        adapter.reconcile(&attempt, None).await.unwrap(),
        Reconciliation::Unavailable { .. }
    ));
}

#[tokio::test]
async fn kept_private_uploads_are_explained() {
    let mut f = Fixture::new().await;
    f.account.capabilities.private_only = false;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(video_json(
            "vid1",
            "processed",
            "succeeded",
            "private",
        )))
        .mount(&f.server)
        .await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let media = YouTubeMedia {
        id: "vid1".into(),
        account_id: f.account.id,
        privacy: "public".into(),
        uploaded_at: t0(),
    };
    let post = adapter.publish(&f.submission(), &media).await.unwrap();
    assert!(post.note.unwrap().contains("kept this video private"));
}

#[tokio::test]
async fn validation_applies_the_shorts_rules() {
    let f = Fixture::new().await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let wide = MediaProperties {
        duration_ms: 200_000,
        width: 1920,
        height: 1080,
        ..Default::default()
    };
    let gif = f.file.with_extension("gif");
    let content = RevisionContent::new(
        VariantId::new(),
        1,
        "Use <b> tags",
        &[],
        Destination {
            account_id: f.account.id,
            platform: Platform::YoutubeShorts,
        },
    )
    .unwrap();
    let p = Publication::draft(AssetId::new(), content, None, t0());
    let sub = Submission {
        publication: &p,
        account: &f.account,
        media_path: &gif,
        media: &wide,
        size_bytes: SIZE,
    };
    let issues = adapter.validate(&sub).await.unwrap();
    assert!(issues.iter().any(|i| matches!(
        i,
        ValidationIssue::TooLong {
            max_ms: 180_000,
            ..
        }
    )));
    assert!(
        issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::NotVertical { .. }))
    );
    assert!(
        issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::UnsupportedFormat { .. }))
    );
    assert!(
        issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::ForbiddenCharacters { .. }))
    );
}

#[tokio::test]
async fn capabilities_describe_private_only_and_quota() {
    let f = Fixture::new().await;
    let adapter = f.adapter(Tokens::new(&["tok"]));
    let c = adapter.capabilities(&f.account).await.unwrap();
    assert!(c.can_publish_automatically());
    assert!(c.private_only);
    assert!(c.notes.iter().any(|n| n.contains("private")));
    assert!(c.notes.iter().any(|n| n.contains("midnight Pacific")));
    let signed_out = f.adapter(Tokens::new(&[]));
    let c = signed_out.capabilities(&f.account).await.unwrap();
    assert!(!c.can_publish_automatically());
}
