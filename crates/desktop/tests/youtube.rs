//! The YouTube connector end to end, against wiremock fakes of Google's token endpoint and the
//! YouTube Data API: connect a channel through the loopback redirect, let a watch-folder video
//! enter a collection whose rule targets the channel, approve it, and let the worker upload it.
//! Nothing here calls Google.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shorts_adapters::google::GoogleEndpoints;
use shorts_adapters::youtube::QuotaLimits;
use shorts_desktop::credentials::{CredentialStore, MemoryCredentials};
use shorts_desktop::views::Action;
use shorts_desktop::youtube::{YouTubeConnectProgress, YouTubeConnector, YouTubeSetup};
use shorts_desktop::{AssetUpdate, PermissionInput, PreferencesUpdate, RuleInput, ScheduleInput};
use shorts_domain::{
    AccountId, PermissionStatus, Platform, PublicationId, PublicationState, RuleOutput,
};
use shorts_scheduler::Disposition;
use shorts_store::PublicationFilter;
use support::{Harness, harness};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "123-test.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "GOCSPX-test-secret";

struct World {
    h: Harness,
    server: MockServer,
    credentials: Arc<MemoryCredentials>,
    youtube: Arc<YouTubeConnector>,
}

async fn world() -> World {
    let h = harness().await;
    let server = MockServer::start().await;
    let base = server.uri();
    let credentials = Arc::new(MemoryCredentials::default());
    let youtube = YouTubeConnector::new(
        h.app.clone(),
        YouTubeSetup {
            google: GoogleEndpoints {
                auth: format!("{base}/o/oauth2/v2/auth").parse().unwrap(),
                token: format!("{base}/token").parse().unwrap(),
                revoke: format!("{base}/revoke").parse().unwrap(),
            },
            api_base: base.parse().unwrap(),
            limits: QuotaLimits::default(),
            request_timeout: Duration::from_millis(500),
            chunk_bytes: 256 * 1024,
            poll_every: Duration::from_millis(1),
        },
        credentials.clone(),
    );
    World {
        h,
        server,
        credentials,
        youtube,
    }
}

fn token_response(access: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "access_token": access,
        "expires_in": 3599,
        "refresh_token": "1//refresh",
        "refresh_token_expires_in": 604_799,
        "scope": "https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly",
        "token_type": "Bearer"
    }))
}

impl World {
    async fn mock_sign_in(&self) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .respond_with(token_response("ya29.first"))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/youtube/v3/channels"))
            .and(query_param("part", "snippet"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{ "id": "UCtestchannel", "snippet": { "title": "Test Kitchen" } }]
            })))
            .mount(&self.server)
            .await;
    }

    /// Plays the browser: opens the consent URL's redirect with a code for its state.
    async fn sign_in(&self, account: Option<AccountId>) -> YouTubeConnectProgress {
        let consent = self.youtube.begin_connect(account).await.unwrap();
        let q: std::collections::HashMap<_, _> = consent.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], CLIENT_ID);
        assert!(!consent.as_str().contains(CLIENT_SECRET));
        let redirect = format!("{}/?state={}&code=4%2Fcode", q["redirect_uri"], q["state"]);
        let page = reqwest::get(redirect).await.unwrap();
        assert!(page.status().is_success());
        for _ in 0..200 {
            match self.youtube.connect_progress() {
                YouTubeConnectProgress::Pending => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                done => return done,
            }
        }
        panic!("the sign-in never finished");
    }

    async fn connect(&self) -> AccountId {
        self.youtube
            .set_client(CLIENT_ID, CLIENT_SECRET)
            .await
            .unwrap();
        self.mock_sign_in().await;
        let YouTubeConnectProgress::Connected { account } = self.sign_in(None).await else {
            panic!("not connected")
        };
        account.id.parse().unwrap()
    }

    /// A collection whose rule targets `account`, fed by the watch folder.
    async fn route_to(&self, account: AccountId) {
        let c = self.h.app.create_collection("To YouTube").await.unwrap();
        let id = c.id.parse().unwrap();
        self.h
            .app
            .create_rule(
                id,
                RuleInput {
                    name: "Shorts".into(),
                    enabled: true,
                    destinations: vec![account],
                    output: RuleOutput::AwaitingApproval,
                    caption_template: "{title}".into(),
                    hashtags: vec!["shorts".into()],
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

    /// A watch-folder file whose rule-made publication is approved to go out now.
    async fn approved_upload(&self) -> PublicationId {
        let file = self.h.video("bagels.mp4", &vec![7u8; 300 * 1024]);
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
        assert_eq!(pubs.len(), 1, "the rule made one YouTube publication");
        let p = &pubs[0];
        assert_eq!(p.platform(), Platform::YoutubeShorts);
        assert_eq!(p.state(), PublicationState::AwaitingApproval);
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

    async fn mock_upload(&self) {
        Mock::given(method("POST"))
            .and(path("/upload/youtube/v3/videos"))
            .and(query_param("uploadType", "resumable"))
            .respond_with(ResponseTemplate::new(200).insert_header(
                "Location",
                format!("{}/upload/session?upload_id=xyz", self.server.uri()),
            ))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/upload/session"))
            .and(header("Content-Range", "bytes 0-262143/307200"))
            .respond_with(ResponseTemplate::new(308).insert_header("Range", "bytes=0-262143"))
            .mount(&self.server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/upload/session"))
            .and(header("Content-Range", "bytes 262144-307199/307200"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "vid1" })))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/youtube/v3/videos"))
            .and(query_param("part", "status,processingDetails"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{ "id": "vid1",
                    "status": { "uploadStatus": "uploaded", "privacyStatus": "private" },
                    "processingDetails": { "processingStatus": "processing" } }]
            })))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    async fn mock_processed(&self, delay: Option<Duration>) {
        let ok = ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": "vid1",
                "status": { "uploadStatus": "processed", "privacyStatus": "private" },
                "processingDetails": { "processingStatus": "succeeded" } }]
        }));
        if let Some(delay) = delay {
            // The publish confirmation's answer never arrives in time.
            Mock::given(method("GET"))
                .and(path("/youtube/v3/videos"))
                .and(query_param("part", "status"))
                .respond_with(ok.clone().set_delay(delay))
                .up_to_n_times(1)
                .with_priority(2)
                .mount(&self.server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/youtube/v3/videos"))
            .respond_with(ok)
            .mount(&self.server)
            .await;
    }
}

#[tokio::test]
async fn a_watch_folder_video_goes_through_a_rule_and_approval_to_the_channel() {
    let w = world().await;
    let before = w.youtube.status().await.unwrap();
    assert!(!before.client_configured);
    assert!(before.next_step.is_some());

    let account = w.connect().await;
    // Secrets stay in the credential store; the database only names the entry.
    let stored = w.h.app.store().accounts().require(account).await.unwrap();
    assert_eq!(stored.credential_ref, Some(format!("youtube:{account}")));
    assert!(stored.capabilities.can_publish_automatically());
    assert!(stored.capabilities.private_only, "private until audited");
    let raw = serde_json::to_string(&stored).unwrap();
    assert!(!raw.contains("1//refresh") && !raw.contains("UCtestchannel"));
    assert!(
        w.credentials
            .get(&format!("youtube:{account}"))
            .unwrap()
            .unwrap()
            .contains("1//refresh")
    );
    let status = w.youtube.status().await.unwrap();
    assert!(status.client_configured);
    assert_eq!(status.channels.len(), 1);
    let channel = &status.channels[0];
    assert_eq!(channel.display_name, "Test Kitchen");
    assert!(channel.connected && channel.private_only && !channel.needs_reconnect);
    assert!(channel.sign_in_expires_at.is_some());
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("UCtestchannel")
    );

    w.route_to(account).await;
    w.mock_upload().await;
    w.mock_processed(None).await;
    let id = w.approved_upload().await;
    let ran = w.h.drain().await;
    assert_eq!(ran, vec![Disposition::Succeeded], "{ran:?}");

    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert_eq!(
        view.published_url.as_deref(),
        Some("https://www.youtube.com/shorts/vid1")
    );
    assert!(view.published_note.unwrap().contains("private"));
    let status = w.youtube.status().await.unwrap();
    assert_eq!(status.quota.uploads_used, 1);
    assert!(status.quota.units_used >= 3);
}

#[tokio::test]
async fn an_expired_sign_in_waits_for_reconnect_and_then_continues() {
    let w = world().await;
    let account = w.connect().await;
    w.route_to(account).await;
    // Google ends the grant (7 days in Testing, or revoked).
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Token has been expired or revoked."
        })))
        .mount(&w.server)
        .await;
    // Past the access token's hour.
    w.h.clock.advance(chrono::Duration::hours(2));
    let id = w.approved_upload().await;
    w.h.drain().await;

    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::NeedsAuthentication);
    assert!(view.actions.contains(&Action::Reconnect));
    assert!(
        view.status_text.contains("reconnect"),
        "{}",
        view.status_text
    );
    let status = w.youtube.status().await.unwrap();
    assert!(status.channels[0].needs_reconnect);

    w.mock_upload().await;
    w.mock_processed(None).await;
    let YouTubeConnectProgress::Connected { .. } = w.sign_in(Some(account)).await else {
        panic!("reconnect failed")
    };
    assert!(!w.youtube.status().await.unwrap().channels[0].needs_reconnect);
    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::RetryScheduled);
    w.h.drain().await;
    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(
        view.state,
        PublicationState::Published,
        "{}",
        view.status_text
    );
}

#[tokio::test]
async fn a_lost_publish_answer_is_reconciled_instead_of_left_unknown() {
    let w = world().await;
    let account = w.connect().await;
    w.route_to(account).await;
    w.mock_upload().await;
    w.mock_processed(Some(Duration::from_secs(3))).await;
    let id = w.approved_upload().await;
    w.h.drain().await;
    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(
        view.state,
        PublicationState::Published,
        "{}",
        view.status_text
    );
    assert_eq!(
        view.published_url.as_deref(),
        Some("https://www.youtube.com/shorts/vid1")
    );
}

#[tokio::test]
async fn quota_exceeded_reschedules_after_the_pacific_reset() {
    let w = world().await;
    let account = w.connect().await;
    w.route_to(account).await;
    Mock::given(method("POST"))
        .and(path("/upload/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": { "code": 403, "message": "quota", "errors": [{ "reason": "quotaExceeded" }] }
        })))
        .mount(&w.server)
        .await;
    let id = w.approved_upload().await;
    w.h.drain().await;
    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::RetryScheduled);
    // The harness clock is 2026-10-30 12:00 UTC; the quota resets at 07:00 UTC on the 31st.
    let retry = view.retry_at.unwrap();
    assert!(
        retry
            >= "2026-10-31T07:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
    );
    let status = w.youtube.status().await.unwrap();
    assert!(status.quota.warning.unwrap().contains("used up"));
}

#[tokio::test]
async fn disconnecting_keeps_the_destination_on_export_packages() {
    let w = world().await;
    let account = w.connect().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&w.server)
        .await;
    let status = w.youtube.disconnect(account).await.unwrap();
    assert!(!status.channels[0].connected);
    assert!(
        w.credentials
            .get(&format!("youtube:{account}"))
            .unwrap()
            .is_none()
    );
    let stored = w.h.app.store().accounts().require(account).await.unwrap();
    assert!(!stored.capabilities.can_publish_automatically());
    assert!(stored.credential_ref.is_none());

    // Public uploads can be switched on once the project is audited, and reconnecting keeps
    // that choice.
    let status = w.youtube.set_public_uploads(account, true).await.unwrap();
    assert!(!status.channels[0].private_only);
    let YouTubeConnectProgress::Connected { account: view } = w.sign_in(Some(account)).await else {
        panic!("reconnect failed")
    };
    assert!(view.connected && !view.private_only);
}

#[tokio::test]
async fn an_export_package_destination_can_be_upgraded_to_a_connected_channel() {
    let w = world().await;
    let manual =
        w.h.app
            .add_manual_account(Platform::YoutubeShorts, "My channel")
            .await
            .unwrap();
    w.youtube
        .set_client(CLIENT_ID, CLIENT_SECRET)
        .await
        .unwrap();
    w.mock_sign_in().await;
    let id: AccountId = manual.id.parse().unwrap();
    let YouTubeConnectProgress::Connected { account } = w.sign_in(Some(id)).await else {
        panic!("not connected")
    };
    assert_eq!(account.id, manual.id);
    assert!(account.automatic && account.private_only);
    // Connecting the same channel again finds the existing destination.
    let YouTubeConnectProgress::Connected { account: again } = w.sign_in(None).await else {
        panic!("not connected")
    };
    assert_eq!(again.id, manual.id);
    assert_eq!(w.h.app.list_accounts().await.unwrap().len(), 1);
}
