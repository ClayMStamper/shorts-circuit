//! TikTok new-post detection against a local fake of the OAuth broker and the Display API
//! (wiremock). No real credentials, and no request ever asks TikTok for a video file.

mod support;

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Duration;
use reqwest::Url;
use serde_json::{Value, json};
use shorts_adapters::broker::challenge_for;
use shorts_adapters::tiktok_oauth::{ChallengeEncoding, TikTokEndpoints};
use shorts_desktop::credentials::{CredentialStore, MemoryCredentials};
use shorts_desktop::tiktok::{ConnectProgress, TikTokIntake, TikTokSetup, TikTokStatus};
use shorts_desktop::{AppEvent, ImportOutcome, LibraryQuery, RuleInput};
use shorts_domain::{AssetId, Platform, PublicationState, RuleOutput};
use shorts_store::PublicationFilter;
use support::{Harness, harness};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn setup(server: &MockServer) -> TikTokSetup {
    TikTokSetup {
        broker_url: Some(Url::parse(&server.uri()).unwrap()),
        ..direct_setup(server)
    }
}

/// No broker: the owner's own TikTok app against a fake TikTok.
fn direct_setup(server: &MockServer) -> TikTokSetup {
    let base = server.uri();
    TikTokSetup {
        broker_url: None,
        api_base: Url::parse(&base).unwrap(),
        oauth: TikTokEndpoints {
            authorize: Url::parse(&format!("{base}/v2/auth/authorize/")).unwrap(),
            token: Url::parse(&format!("{base}/v2/oauth/token/")).unwrap(),
            revoke: Url::parse(&format!("{base}/v2/oauth/revoke/")).unwrap(),
        },
        loopback_port: 0,
        challenge: ChallengeEncoding::Hex,
        poll_every: StdDuration::from_secs(3600),
    }
}

fn tokens(access: &str) -> Value {
    json!({
        "access_token": access,
        "expires_in": 86400,
        "open_id": "test-open-id",
        "refresh_expires_in": 31536000,
        "refresh_token": "test-refresh-token",
        "scope": "user.info.basic,video.list"
    })
}

fn ok<T: serde::Serialize>(data: T) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": data,
        "error": { "code": "ok", "message": "", "log_id": "test" }
    }))
}

async fn broker_configured(server: &MockServer, configured: bool) {
    Mock::given(method("GET"))
        .and(path("/v1/tiktok/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "configured": configured })))
        .mount(server)
        .await;
}

async fn user_info(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v2/user/info/"))
        .and(header("authorization", "Bearer test-access-token"))
        .respond_with(ok(
            json!({ "user": { "open_id": "test-open-id", "display_name": "Test Baker" } }),
        ))
        .mount(server)
        .await;
}

/// 2026-10-29 18:00Z, 20 s (the fake media backend probes every file as 20 s).
const POST_A: i64 = 1_793_296_800;
/// 2026-10-30 08:00Z, 45 s.
const POST_B: i64 = 1_793_347_200;
/// 2026-10-01: older than the first check looks back.
const POST_OLD: i64 = 1_790_848_800;

fn videos(server: &MockServer) -> Value {
    json!({
        "videos": [
            {
                "id": "7000000000000000002",
                "title": "",
                "video_description": "Overnight focaccia\n#bread #baking",
                "duration": 45,
                "share_url": "https://www.tiktok.com/@test/video/7000000000000000002",
                "create_time": POST_B
            },
            {
                "id": "7000000000000000001",
                "title": "Bagels at dawn",
                "video_description": "Boiled, then baked #bagels",
                "duration": 20,
                "cover_image_url": format!("{}/covers/a.jpeg", server.uri()),
                "share_url": "https://www.tiktok.com/@test/video/7000000000000000001",
                "create_time": POST_A
            },
            {
                "id": "6999999999999999999",
                "title": "Old one",
                "duration": 20,
                "create_time": POST_OLD
            }
        ],
        "cursor": 1_790_848_800_000_i64,
        "has_more": false
    })
}

async fn video_list(server: &MockServer, token: &str) {
    Mock::given(method("POST"))
        .and(path("/v2/video/list/"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ok(videos(server)))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/covers/a.jpeg"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(vec![0xFF, 0xD8, 0xFF, 0xE0]),
        )
        .mount(server)
        .await;
}

/// A connected intake: broker configured, sign-in claimed, profile read.
async fn connected(
    h: &Harness,
    server: &MockServer,
) -> (Arc<TikTokIntake>, Arc<MemoryCredentials>) {
    broker_configured(server, true).await;
    user_info(server).await;
    Mock::given(method("POST"))
        .and(path("/v1/tiktok/claim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens("test-access-token")))
        .mount(server)
        .await;
    let credentials = Arc::new(MemoryCredentials::default());
    let intake = TikTokIntake::new(h.app.clone(), Some(setup(server)), credentials.clone());
    intake.set_enabled(true).await.unwrap();
    intake.begin_connect().await.unwrap();
    assert!(matches!(
        intake.finish_connect().await.unwrap(),
        ConnectProgress::Connected { .. }
    ));
    (intake, credentials)
}

#[tokio::test]
async fn without_a_developer_app_the_feature_is_unavailable_with_the_next_step() {
    let h = harness().await;
    let intake = TikTokIntake::new(h.app.clone(), None, Arc::new(MemoryCredentials::default()));
    match intake.status().await.unwrap() {
        TikTokStatus::Unavailable { next_step, .. } => {
            assert!(next_step.contains("Register a TikTok developer app"))
        }
        other => panic!("{other:?}"),
    }
    assert!(intake.set_enabled(true).await.is_err());

    // A broker without a TikTok app says the same.
    let server = MockServer::start().await;
    broker_configured(&server, false).await;
    let intake = TikTokIntake::new(
        h.app.clone(),
        Some(setup(&server)),
        Arc::new(MemoryCredentials::default()),
    );
    assert!(matches!(
        intake.status().await.unwrap(),
        TikTokStatus::Unavailable { .. }
    ));
}

#[tokio::test]
async fn connecting_goes_through_the_broker_with_a_verifier() {
    let h = harness().await;
    let server = MockServer::start().await;
    broker_configured(&server, true).await;
    user_info(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/tiktok/claim"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "status": "pending" })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/tiktok/claim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens("test-access-token")))
        .mount(&server)
        .await;
    let credentials = Arc::new(MemoryCredentials::default());
    let intake = TikTokIntake::new(h.app.clone(), Some(setup(&server)), credentials.clone());

    // Off until the owner turns it on.
    assert_eq!(intake.status().await.unwrap(), TikTokStatus::Off);
    assert!(intake.begin_connect().await.is_err());
    assert_eq!(
        intake.set_enabled(true).await.unwrap(),
        TikTokStatus::NotConnected
    );

    let url = intake.begin_connect().await.unwrap();
    assert_eq!(url.path(), "/v1/tiktok/connect");
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(intake.status().await.unwrap(), TikTokStatus::Connecting);

    assert_eq!(
        intake.finish_connect().await.unwrap(),
        ConnectProgress::Pending
    );
    let ConnectProgress::Connected { status } = intake.finish_connect().await.unwrap() else {
        panic!("expected to connect")
    };
    match status {
        TikTokStatus::Connected {
            display_name,
            pending_posts,
            ..
        } => {
            assert_eq!(display_name, "Test Baker");
            assert_eq!(pending_posts, 0);
        }
        other => panic!("{other:?}"),
    }

    // The claim proved possession of the verifier behind the challenge in the browser URL.
    let claims: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/tiktok/claim")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(claims[0]["state"], query["state"].as_str());
    assert_eq!(
        challenge_for(claims[0]["verifier"].as_str().unwrap()),
        query["challenge"]
    );
    // Tokens are in the credential store, not in settings.
    let stored = credentials.get("tiktok").unwrap().unwrap();
    assert!(stored.contains("test-access-token"));
    let settings: Option<Value> = h.app.store().settings().get("tiktok_source").await.unwrap();
    assert!(!settings.unwrap().to_string().contains("test-access-token"));
}

#[tokio::test]
async fn the_owners_own_app_signs_in_directly_without_a_broker() {
    let h = harness().await;
    let server = MockServer::start().await;
    user_info(&server).await;
    Mock::given(method("POST"))
        .and(path("/v2/oauth/token/"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_secret=test-secret"))
        .and(body_string_contains("code=test-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens("test-access-token")))
        .mount(&server)
        .await;
    let credentials = Arc::new(MemoryCredentials::default());
    let intake = TikTokIntake::new(
        h.app.clone(),
        Some(direct_setup(&server)),
        credentials.clone(),
    );

    // Without the app the next step says what to register and paste.
    match intake.status().await.unwrap() {
        TikTokStatus::Unavailable { next_step, .. } => {
            assert!(
                next_step.contains("127.0.0.1:47616/callback/"),
                "{next_step}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert!(intake.set_app("key", " ").await.is_err());
    assert_eq!(
        intake.set_app("test-key", "test-secret").await.unwrap(),
        TikTokStatus::Off
    );
    let info = intake.app_info().unwrap();
    assert!(info.app_configured);
    assert_eq!(info.client_key.as_deref(), Some("test-key"));
    assert!(
        !serde_json::to_string(&info)
            .unwrap()
            .contains("test-secret")
    );
    intake.set_enabled(true).await.unwrap();

    // A second Connect (e.g. after pasting a new key) takes the port over from the first.
    intake.begin_connect().await.unwrap();
    let url = intake.begin_connect().await.unwrap();
    assert_eq!(url.path(), "/v2/auth/authorize/");
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_key"], "test-key");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!url.as_str().contains("test-secret"));
    assert_eq!(
        intake.finish_connect().await.unwrap(),
        ConnectProgress::Pending
    );

    // The browser comes back to the loopback address with the code.
    let redirect = Url::parse(&query["redirect_uri"]).unwrap();
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), "/callback/");
    let mut back = redirect.clone();
    back.query_pairs_mut()
        .append_pair("state", &query["state"])
        .append_pair("code", "test-code");
    let page = reqwest::get(back).await.unwrap();
    assert!(page.status().is_success());

    let status = tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            match intake.finish_connect().await.unwrap() {
                ConnectProgress::Pending => tokio::time::sleep(StdDuration::from_millis(20)).await,
                ConnectProgress::Connected { status } => return status,
                ConnectProgress::Failed { message } => panic!("{message}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(status, TikTokStatus::Connected { .. }),
        "{status:?}"
    );

    // The exchange proved the verifier behind the (hex) challenge.
    let exchange = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/v2/oauth/token/")
        .unwrap();
    let body = String::from_utf8(exchange.body).unwrap();
    let form: std::collections::HashMap<String, String> =
        Url::parse(&format!("http://form.invalid/?{body}"))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
    assert_eq!(
        ChallengeEncoding::Hex.challenge(&form["code_verifier"]),
        query["code_challenge"]
    );
    assert_eq!(form["redirect_uri"], query["redirect_uri"]);
    assert!(
        credentials
            .get("tiktok")
            .unwrap()
            .unwrap()
            .contains("test-access-token")
    );
}

#[tokio::test]
async fn new_posts_become_items_awaiting_media_once() {
    let h = harness().await;
    let server = MockServer::start().await;
    video_list(&server, "test-access-token").await;
    let (intake, _) = connected(&h, &server).await;

    let report = intake.check_now().await.unwrap();
    assert_eq!(report.new_posts, 2);
    let items = h
        .app
        .list_assets(&LibraryQuery {
            awaiting_media: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
    let bagels = items.iter().find(|a| a.title == "Bagels at dawn").unwrap();
    assert_eq!(bagels.creator.as_deref(), Some("Test Baker"));
    assert_eq!(bagels.notes, "Boiled, then baked #bagels");
    assert_eq!(
        bagels.source_url.as_deref(),
        Some("https://www.tiktok.com/@test/video/7000000000000000001")
    );
    let post = bagels.source_post.as_ref().unwrap();
    assert_eq!(post.duration_ms, Some(20_000));
    assert_eq!(post.posted_at.timestamp(), POST_A);
    // The cover image was copied (TikTok's link expires) and serves as the thumbnail.
    let cover = h
        .app
        .thumbnail(bagels.id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(cover.starts_with(&h.app.paths().cache));
    assert_eq!(std::fs::read(&cover).unwrap(), [0xFF, 0xD8, 0xFF, 0xE0]);
    // No title: the caption's first line.
    assert!(items.iter().any(|a| a.title == "Overnight focaccia"));

    // Checking again finds nothing new.
    assert_eq!(intake.check_now().await.unwrap().new_posts, 0);
    assert_eq!(
        h.app
            .list_assets(&LibraryQuery::default())
            .await
            .unwrap()
            .len(),
        2
    );
    // Only metadata endpoints and the cover image were requested.
    for r in server.received_requests().await.unwrap() {
        let p = r.url.path();
        assert!(
            p.starts_with("/v1/tiktok/") || p.starts_with("/v2/") || p.starts_with("/covers/"),
            "unexpected request to {p}"
        );
    }
}

async fn import_from_watch_folder(h: &Harness, name: &str) -> AssetId {
    let path = h.video(name, format!("original {name}").as_bytes());
    match h.app.import_watched(&path).await.unwrap() {
        ImportOutcome::Imported { asset_id } => asset_id,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn arriving_files_are_suggested_and_merged_only_when_confirmed() {
    let h = harness().await;
    let server = MockServer::start().await;
    video_list(&server, "test-access-token").await;
    let (intake, _) = connected(&h, &server).await;
    intake.check_now().await.unwrap();

    // The bagels post sits in a collection whose rule posts to YouTube.
    let account: shorts_domain::AccountId = h
        .app
        .add_manual_account(Platform::YoutubeShorts, "Channel")
        .await
        .unwrap()
        .id
        .parse()
        .unwrap();
    let c = h.app.create_collection("TikTok repost").await.unwrap();
    let c_id = c.id.parse().unwrap();
    h.app
        .create_rule(
            c_id,
            RuleInput {
                name: "To Shorts".into(),
                enabled: true,
                destinations: vec![account],
                output: RuleOutput::AwaitingApproval,
                caption_template: "{caption}".into(),
                hashtags: vec![],
                include_asset_tags: false,
                // The post went up yesterday; a rule starting now would ignore it.
                effective_from: Some(support::t0() - Duration::days(2)),
            },
        )
        .await
        .unwrap();
    let pending = h.app.match_suggestions().await.unwrap();
    assert!(pending.is_empty());
    let items = h.app.list_assets(&LibraryQuery::default()).await.unwrap();
    let bagels_item: AssetId = items
        .iter()
        .find(|a| a.title == "Bagels at dawn")
        .unwrap()
        .id
        .parse()
        .unwrap();
    // Awaiting media: the rule can't fire yet.
    assert!(
        h.app
            .add_to_collection(c_id, bagels_item)
            .await
            .unwrap()
            .is_empty()
    );

    // Two 20 s files arrive 18 h after the 20 s post: both are suggested for it, none for the
    // 45 s post. Nothing is merged on its own.
    let mut rx = h.app.subscribe();
    let first = import_from_watch_folder(&h, "IMG_0001.mov").await;
    let second = import_from_watch_folder(&h, "IMG_0002.mov").await;
    let mut suggested = false;
    while let Ok(event) = rx.try_recv() {
        suggested |= matches!(event, AppEvent::MatchSuggested { asset_id: Some(ref a) } if *a == first.to_string());
    }
    assert!(suggested);
    let suggestions = h.app.match_suggestions().await.unwrap();
    assert_eq!(suggestions.len(), 2);
    assert!(
        suggestions
            .iter()
            .all(|s| s.post.video_id == "7000000000000000001")
    );
    assert!(suggestions.iter().all(|s| s.duration_diff_ms == 0));
    assert!((suggestions[0].hours_apart - 18.0).abs() < 0.01);
    assert_eq!(
        h.app
            .list_assets(&LibraryQuery::default())
            .await
            .unwrap()
            .len(),
        4
    );

    // The user turns one down; it isn't offered again.
    h.app
        .dismiss_match("7000000000000000001", second)
        .await
        .unwrap();
    let suggestions = h.app.match_suggestions().await.unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].candidate.asset_id, first.to_string());

    // Confirming merges: the file takes the post's details and collections, the awaiting
    // item goes away, and the collection's rule fires for the file (still needing approval).
    let merged = h
        .app
        .confirm_match("7000000000000000001", first)
        .await
        .unwrap();
    assert_eq!(merged.title, "Bagels at dawn");
    assert_eq!(merged.notes, "Boiled, then baked #bagels");
    assert!(merged.source_post.as_ref().unwrap().matched);
    assert!(
        h.app
            .store()
            .assets()
            .get(bagels_item)
            .await
            .unwrap()
            .is_none()
    );
    assert!(h.app.match_suggestions().await.unwrap().is_empty());
    let detail = h.app.asset_detail(first).await.unwrap();
    assert_eq!(detail.collections[0].name, "TikTok repost");
    let pubs = h
        .app
        .store()
        .publications()
        .list(&PublicationFilter {
            asset_id: Some(first),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(pubs.len(), 1);
    assert_eq!(pubs[0].state(), PublicationState::AwaitingApproval);
    assert_eq!(pubs[0].content().caption, "Boiled, then baked #bagels");

    // A matched file can't be matched again, and the post can't be matched twice.
    assert!(
        h.app
            .confirm_match("7000000000000000001", second)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn expiring_tokens_are_refreshed_through_the_broker() {
    let h = harness().await;
    let server = MockServer::start().await;
    video_list(&server, "fresh-access-token").await;
    Mock::given(method("POST"))
        .and(path("/v1/tiktok/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens("fresh-access-token")))
        .expect(1)
        .mount(&server)
        .await;
    let (intake, credentials) = connected(&h, &server).await;
    h.clock.advance(Duration::hours(25));
    assert_eq!(intake.check_now().await.unwrap().new_posts, 2);
    assert!(
        credentials
            .get("tiktok")
            .unwrap()
            .unwrap()
            .contains("fresh-access-token")
    );
}

#[tokio::test]
async fn revoked_access_asks_to_reconnect_and_disconnect_keeps_items() {
    let h = harness().await;
    let server = MockServer::start().await;
    video_list(&server, "test-access-token").await;
    let (intake, credentials) = connected(&h, &server).await;
    intake.check_now().await.unwrap();

    server.reset().await;
    broker_configured(&server, true).await;
    Mock::given(method("POST"))
        .and(path("/v2/video/list/"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "data": {},
            "error": { "code": "access_token_invalid", "message": "The access token is invalid.", "log_id": "test" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/tiktok/revoke"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let err = intake.check_now().await.unwrap_err();
    assert_eq!(err.kind(), "intake");
    assert!(matches!(
        intake.status().await.unwrap(),
        TikTokStatus::Reconnect { .. }
    ));

    assert_eq!(
        intake.disconnect().await.unwrap(),
        TikTokStatus::NotConnected
    );
    assert!(credentials.get("tiktok").unwrap().is_none());
    assert_eq!(
        h.app
            .list_assets(&LibraryQuery::default())
            .await
            .unwrap()
            .len(),
        2
    );
}
