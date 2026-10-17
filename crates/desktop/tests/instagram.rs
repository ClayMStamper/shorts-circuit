//! The Instagram connector end to end, against a wiremock fake of the Instagram Graph API and
//! upload host: connect an account with a pasted token, let a watch-folder video enter a
//! collection whose rule targets it, approve it, and let the worker publish the reel. Also
//! token renewal and expiry. Nothing here calls Meta.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shorts_desktop::credentials::{CredentialStore, MemoryCredentials};
use shorts_desktop::instagram::{InstagramConnector, InstagramSetup};
use shorts_desktop::{AssetUpdate, PermissionInput, PreferencesUpdate, RuleInput, ScheduleInput};
use shorts_domain::{
    AccountId, PermissionStatus, Platform, PublicationId, PublicationState, RuleOutput,
};
use shorts_scheduler::Disposition;
use shorts_store::PublicationFilter;
use support::{Harness, harness};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "IGAAtest-pasted-long-lived-token";
const USER: &str = "17841400000000001";

struct World {
    h: Harness,
    server: MockServer,
    credentials: Arc<MemoryCredentials>,
    instagram: Arc<InstagramConnector>,
}

async fn world() -> World {
    let h = harness().await;
    let server = MockServer::start().await;
    let base = server.uri();
    let credentials = Arc::new(MemoryCredentials::default());
    let instagram = InstagramConnector::new(
        h.app.clone(),
        InstagramSetup {
            graph_base: base.parse().unwrap(),
            upload_base: base.parse().unwrap(),
            api_version: "v24.0".into(),
            request_timeout: Duration::from_millis(500),
            upload_timeout: Duration::from_millis(500),
            poll_every: Duration::from_millis(1),
        },
        credentials.clone(),
    );
    World {
        h,
        server,
        credentials,
        instagram,
    }
}

impl World {
    async fn mock_profile(&self, token: &str, account_type: &str) {
        Mock::given(method("GET"))
            .and(path("/v24.0/me"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "app-scoped", "user_id": USER, "username": "testkitchen",
                "account_type": account_type
            })))
            .mount(&self.server)
            .await;
    }

    async fn connect(&self) -> AccountId {
        self.mock_profile(TOKEN, "MEDIA_CREATOR").await;
        let view = self.instagram.connect(TOKEN, None).await.unwrap();
        view.id.parse().unwrap()
    }

    async fn route_to(&self, account: AccountId) {
        let c = self.h.app.create_collection("To Instagram").await.unwrap();
        let id = c.id.parse().unwrap();
        self.h
            .app
            .create_rule(
                id,
                RuleInput {
                    name: "Reels".into(),
                    enabled: true,
                    destinations: vec![account],
                    output: RuleOutput::AwaitingApproval,
                    caption_template: "{title}".into(),
                    hashtags: vec!["reels".into()],
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
        let file = self.h.video("bagels.mp4", &vec![7u8; 100 * 1024]);
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
        assert_eq!(pubs.len(), 1, "the rule made one Instagram publication");
        let p = &pubs[0];
        assert_eq!(p.platform(), Platform::InstagramReels);
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

    async fn mock_publishing(&self) {
        Mock::given(method("POST"))
            .and(path(format!("/v24.0/{USER}/media")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "uri": format!("{}/ig-api-upload/v24.0/c1", self.server.uri()),
            })))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ig-api-upload/v24.0/c1"))
            .and(header("authorization", format!("OAuth {TOKEN}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v24.0/c1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "status_code": "FINISHED" })),
            )
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v24.0/{USER}/content_publishing_limit")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "quota_usage": 0, "config": { "quota_total": 100 } }]
            })))
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v24.0/{USER}/media_publish")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "m1" })))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v24.0/m1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "m1", "permalink": "https://www.instagram.com/reel/AbC123/"
            })))
            .mount(&self.server)
            .await;
    }
}

#[tokio::test]
async fn a_watch_folder_video_goes_through_a_rule_and_approval_to_instagram() {
    let w = world().await;
    let before = w.instagram.status().await.unwrap();
    assert!(before.accounts.is_empty());
    assert!(before.next_step.is_some());

    let account = w.connect().await;
    // Secrets stay in the credential store; the database only names the entry.
    let stored = w.h.app.store().accounts().require(account).await.unwrap();
    assert_eq!(stored.display_name, "@testkitchen");
    assert_eq!(stored.credential_ref, Some(format!("instagram:{account}")));
    assert!(stored.capabilities.can_publish_automatically());
    let raw = serde_json::to_string(&stored).unwrap();
    assert!(!raw.contains(TOKEN) && !raw.contains(USER));
    let secret = w
        .credentials
        .get(&format!("instagram:{account}"))
        .unwrap()
        .unwrap();
    assert!(secret.contains(TOKEN) && secret.contains(USER));
    let status = w.instagram.status().await.unwrap();
    assert!(status.next_step.is_none());
    let a = &status.accounts[0];
    assert!(a.connected && !a.needs_reconnect);
    assert!(a.token_expires_at.is_some());
    let shown = serde_json::to_string(&status).unwrap();
    assert!(!shown.contains(TOKEN) && !shown.contains(USER));

    w.route_to(account).await;
    w.mock_publishing().await;
    let id = w.approved_upload().await;
    let ran = w.h.drain().await;
    assert_eq!(ran, vec![Disposition::Succeeded], "{ran:?}");
    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert_eq!(
        view.published_url.as_deref(),
        Some("https://www.instagram.com/reel/AbC123/")
    );
}

#[tokio::test]
async fn personal_accounts_and_bad_tokens_are_refused_with_a_next_step() {
    let w = world().await;
    w.mock_profile(TOKEN, "PERSONAL").await;
    let err = w.instagram.connect(TOKEN, None).await.unwrap_err();
    assert!(err.to_string().contains("professional"), "{err}");

    Mock::given(method("GET"))
        .and(path("/v24.0/me"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "Invalid OAuth access token", "code": 190 }
        })))
        .mount(&w.server)
        .await;
    let err = w
        .instagram
        .connect("IGAA-not-a-real-token-at-all", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Meta app dashboard"), "{err}");
    let err = w.instagram.connect("short", None).await.unwrap_err();
    assert!(err.to_string().contains("whole access token"), "{err}");
    assert!(w.instagram.status().await.unwrap().accounts.is_empty());
}

#[tokio::test]
async fn tokens_are_renewed_before_they_run_out() {
    let w = world().await;
    let account = w.connect().await;
    Mock::given(method("GET"))
        .and(path("/refresh_access_token"))
        .and(query_param("access_token", TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "IGAA-renewed-long-lived-token", "expires_in": 5_184_000
        })))
        .expect(1)
        .mount(&w.server)
        .await;

    // Early on, nothing is due.
    w.h.clock.advance(chrono::Duration::days(2));
    assert_eq!(w.instagram.renew_due().await.unwrap(), 0);

    // With less than 20 days left, the token is renewed for another 60.
    w.h.clock.advance(chrono::Duration::days(40));
    assert_eq!(w.instagram.renew_due().await.unwrap(), 1);
    let secret = w
        .credentials
        .get(&format!("instagram:{account}"))
        .unwrap()
        .unwrap();
    assert!(secret.contains("IGAA-renewed"));
    let status = w.instagram.status().await.unwrap();
    let a = &status.accounts[0];
    assert!(a.renewed_at.is_some());
    assert!(a.token_expires_at.unwrap() > support::t0() + chrono::Duration::days(42 + 59));
}

#[tokio::test]
async fn an_expired_token_asks_for_a_reconnect_and_reconnecting_resumes() {
    let w = world().await;
    let account = w.connect().await;
    w.h.clock.advance(chrono::Duration::days(61));
    assert_eq!(w.instagram.renew_due().await.unwrap(), 0);
    let status = w.instagram.status().await.unwrap();
    assert!(status.accounts[0].needs_reconnect);
    assert!(
        status.accounts[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("ran out")
    );

    // A fresh token for the same account reconnects it.
    w.mock_profile("IGAA-second-pasted-token-value", "BUSINESS")
        .await;
    let view = w
        .instagram
        .connect("IGAA-second-pasted-token-value", Some(account))
        .await
        .unwrap();
    assert_eq!(view.id, account.to_string());
    let status = w.instagram.status().await.unwrap();
    assert_eq!(status.accounts.len(), 1);
    assert!(!status.accounts[0].needs_reconnect);

    // Disconnecting keeps the destination for export packages.
    let status = w.instagram.disconnect(account).await.unwrap();
    assert!(!status.accounts[0].connected);
    assert!(
        w.credentials
            .get(&format!("instagram:{account}"))
            .unwrap()
            .is_none()
    );
}
