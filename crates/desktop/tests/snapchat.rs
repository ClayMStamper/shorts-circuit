//! The Snapchat connector end to end, against wiremock fakes of Snap's accounts and Business
//! API endpoints: sign in through the loopback redirect (or by pasting the code page's text),
//! let a watch-folder video enter a collection whose rule targets the profile, approve it, and
//! let the worker submit it to Spotlight. Also hourly token refresh. Nothing here calls Snap.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shorts_adapters::snapchat::SnapEndpoints;
use shorts_desktop::credentials::{CredentialStore, MemoryCredentials};
use shorts_desktop::snapchat::{ReturnMode, SnapConnectProgress, SnapSetup, SnapchatConnector};
use shorts_desktop::{AssetUpdate, PermissionInput, PreferencesUpdate, RuleInput, ScheduleInput};
use shorts_domain::{
    AccountId, PermissionStatus, Platform, PublicationId, PublicationState, RuleOutput,
};
use shorts_scheduler::Disposition;
use shorts_store::PublicationFilter;
use support::{Harness, harness};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "snap-client-id-123";
const CLIENT_SECRET: &str = "snap-client-secret-456";
const PROFILE: &str = "pp-777";

struct World {
    h: Harness,
    server: MockServer,
    credentials: Arc<MemoryCredentials>,
    snap: Arc<SnapchatConnector>,
}

async fn world() -> World {
    let h = harness().await;
    let server = MockServer::start().await;
    let base = server.uri();
    let credentials = Arc::new(MemoryCredentials::default());
    let snap = SnapchatConnector::new(
        h.app.clone(),
        SnapSetup {
            endpoints: SnapEndpoints {
                authorize: format!("{base}/authorize").parse().unwrap(),
                token: format!("{base}/token").parse().unwrap(),
            },
            api_base: base.parse().unwrap(),
            locale: "en_US".into(),
            request_timeout: Duration::from_millis(500),
            part_timeout: Duration::from_millis(500),
        },
        credentials.clone(),
    );
    World {
        h,
        server,
        credentials,
        snap,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl World {
    async fn mock_sign_in(&self) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains(format!(
                "client_secret={CLIENT_SECRET}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "snap-at-1", "refresh_token": "snap-rt-1", "expires_in": 3600
            })))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/public_profiles/my_profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "request_status": "SUCCESS",
                "public_profile": { "id": PROFILE, "display_name": "Timeless Way", "snap_user_name": "timelessway" }
            })))
            .mount(&self.server)
            .await;
    }

    /// Plays the browser for a loopback sign-in.
    async fn sign_in_loopback(&self) -> SnapConnectProgress {
        let consent = self.snap.begin_connect(None).await.unwrap();
        let q: std::collections::HashMap<_, _> = consent.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], CLIENT_ID);
        assert!(!consent.as_str().contains(CLIENT_SECRET));
        let redirect = format!("{}?state={}&code=abc", q["redirect_uri"], q["state"]);
        assert!(reqwest::get(redirect).await.unwrap().status().is_success());
        for _ in 0..200 {
            match self.snap.connect_progress() {
                SnapConnectProgress::Pending => tokio::time::sleep(Duration::from_millis(10)).await,
                done => return done,
            }
        }
        panic!("the sign-in never finished");
    }

    async fn connect(&self) -> AccountId {
        let redirect = format!("http://127.0.0.1:{}/snapchat/callback", free_port());
        self.snap
            .set_client(CLIENT_ID, CLIENT_SECRET, &redirect)
            .await
            .unwrap();
        self.mock_sign_in().await;
        let SnapConnectProgress::Connected { account } = self.sign_in_loopback().await else {
            panic!("not connected")
        };
        account.id.parse().unwrap()
    }

    async fn route_to(&self, account: AccountId) {
        let c = self.h.app.create_collection("To Spotlight").await.unwrap();
        let id = c.id.parse().unwrap();
        self.h
            .app
            .create_rule(
                id,
                RuleInput {
                    name: "Spotlight".into(),
                    enabled: true,
                    destinations: vec![account],
                    output: RuleOutput::AwaitingApproval,
                    caption_template: "{title}".into(),
                    hashtags: vec!["gamedev".into()],
                    include_asset_tags: false,
                    effective_from: None,
                },
            )
            .await
            .unwrap();
        self.h
            .app
            .update_preferences(PreferencesUpdate {
                watch_collection: Some(Some(id)),
                ..Default::default()
            })
            .await
            .unwrap();
    }

    async fn approved_upload(&self) -> PublicationId {
        let file = self.h.video("spot.mp4", &vec![7u8; 100 * 1024]);
        let asset = self.h.app.import_watched(&file).await.unwrap().asset_id();
        let granted = || {
            Some(PermissionInput {
                status: PermissionStatus::Granted,
                note: None,
            })
        };
        self.h
            .app
            .update_asset(
                asset,
                AssetUpdate {
                    video_permission: granted(),
                    audio_permission: granted(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let pubs = self
            .h
            .app
            .store()
            .publications()
            .list(&PublicationFilter {
                asset_id: Some(asset),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(pubs.len(), 1);
        let p = &pubs[0];
        assert_eq!(p.platform(), Platform::SnapchatSpotlight);
        let id = p.id();
        self.h
            .app
            .render_variant(p.content().variant_id)
            .await
            .unwrap();
        self.h.drain().await;
        let view = self.h.app.schedule(id, ScheduleInput::Now).await.unwrap();
        let review = self.h.app.review(id).await.unwrap();
        assert!(review.can_approve, "{:?}", review.issue_text);
        self.h.app.approve(id, view.revision).await.unwrap();
        id
    }

    async fn mock_posting(&self) {
        Mock::given(method("POST"))
            .and(path(format!("/v1/public_profiles/{PROFILE}/media")))
            .and(header("authorization", "Bearer snap-at-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "request_status": "SUCCESS",
                "media_id": "m1",
                "add_path": format!("/us/v1/public_profiles/{PROFILE}/media/m1/multipart-upload-v2?action=ADD"),
                "finalize_path": format!("/us/v1/public_profiles/{PROFILE}/media/m1/multipart-upload-v2?action=FINALIZE"),
            })))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/us/v1/public_profiles/{PROFILE}/media/m1/multipart-upload-v2"
            )))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "request_status": "SUCCESS" })),
            )
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/public_profiles/{PROFILE}/spotlights")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "request_status": "SUCCESS", "spotlight_id": "spot-1"
            })))
            .expect(1)
            .mount(&self.server)
            .await;
    }
}

#[tokio::test]
async fn a_watch_folder_video_is_submitted_to_spotlight() {
    let w = world().await;
    let before = w.snap.status().await.unwrap();
    assert!(!before.client_configured && before.next_step.is_some());
    assert_eq!(before.return_mode, ReturnMode::Loopback);

    let account = w.connect().await;
    let stored = w.h.app.store().accounts().require(account).await.unwrap();
    assert_eq!(stored.display_name, "Timeless Way");
    assert_eq!(stored.credential_ref, Some(format!("snapchat:{account}")));
    assert!(stored.capabilities.can_publish_automatically());
    let raw = serde_json::to_string(&stored).unwrap();
    assert!(!raw.contains("snap-rt-1") && !raw.contains(PROFILE));
    let status = w.snap.status().await.unwrap();
    assert!(status.client_configured && status.profiles[0].connected);
    let shown = serde_json::to_string(&status).unwrap();
    assert!(!shown.contains(CLIENT_SECRET) && !shown.contains("snap-rt-1"));

    w.route_to(account).await;
    w.mock_posting().await;
    let id = w.approved_upload().await;
    let ran = w.h.drain().await;
    assert_eq!(ran, vec![Disposition::Succeeded], "{ran:?}");
    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert!(view.published_note.unwrap().contains("reviews"));
}

#[tokio::test]
async fn a_web_redirect_page_signs_in_by_pasting_its_code() {
    let w = world().await;
    w.snap
        .set_client(
            CLIENT_ID,
            CLIENT_SECRET,
            "https://example.github.io/shorts-circuit/snap-callback.html",
        )
        .await
        .unwrap();
    w.mock_sign_in().await;
    let consent = w.snap.begin_connect(None).await.unwrap();
    let state = consent
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let status = w.snap.status().await.unwrap();
    assert_eq!(status.return_mode, ReturnMode::Paste);
    assert!(status.awaiting_code && !status.connecting);

    let wrong = format!("{}.abc", "x".repeat(43));
    let err = w.snap.finish_with_code(&wrong).await.unwrap_err();
    assert!(err.to_string().contains("another sign-in"), "{err}");

    let view = w
        .snap
        .finish_with_code(&format!("{state}.abc"))
        .await
        .unwrap();
    assert_eq!(view.display_name, "Timeless Way");
    let status = w.snap.status().await.unwrap();
    assert!(!status.awaiting_code);
    assert_eq!(status.profiles.len(), 1);

    let err = w.snap.finish_with_code("abc").await.unwrap_err();
    assert!(err.to_string().contains("start connecting again"), "{err}");
}

#[tokio::test]
async fn tokens_refresh_hourly_and_a_revoked_grant_asks_for_a_reconnect() {
    let w = world().await;
    let account = w.connect().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=snap-rt-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "snap-at-2", "refresh_token": "snap-rt-2", "expires_in": 3600
        })))
        .up_to_n_times(1)
        .mount(&w.server)
        .await;
    // An hour later the cached token has run out; the next call refreshes and rotates.
    w.h.clock.advance(chrono::Duration::minutes(61));
    let status = w.snap.check_connection(account).await.unwrap();
    assert!(!status.profiles[0].needs_reconnect);
    let grant = w
        .credentials
        .get(&format!("snapchat:{account}"))
        .unwrap()
        .unwrap();
    assert!(
        grant.contains("snap-rt-2"),
        "the rotated refresh token is kept"
    );

    // Snap revokes the grant.
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=snap-rt-2"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant", "error_description": "revoked"
        })))
        .mount(&w.server)
        .await;
    w.h.clock.advance(chrono::Duration::minutes(61));
    w.snap.check_connection(account).await.unwrap();
    let status = w.snap.status().await.unwrap();
    assert!(status.profiles[0].needs_reconnect);
    assert!(
        status.profiles[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("reconnect")
    );

    let status = w.snap.disconnect(account).await.unwrap();
    assert!(!status.profiles[0].connected);
}

#[tokio::test]
async fn the_redirect_address_must_be_https_or_loopback() {
    let w = world().await;
    let err = w
        .snap
        .set_client(CLIENT_ID, CLIENT_SECRET, "http://example.com/callback")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("redirect"), "{err}");
    let err = w
        .snap
        .set_client("short", CLIENT_SECRET, "https://example.com/cb")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("client id"), "{err}");
}
