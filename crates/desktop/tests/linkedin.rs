//! The LinkedIn connector end to end, against a wiremock fake of LinkedIn's REST API: find the
//! pages a pasted token administers, connect one, let a watch-folder video enter a collection
//! whose rule targets it, approve it, and let the worker post it. Also token expiry. Nothing
//! here calls LinkedIn.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shorts_desktop::credentials::{CredentialStore, MemoryCredentials};
use shorts_desktop::linkedin::{LinkedInConnector, LinkedInSetup};
use shorts_desktop::{AssetUpdate, PermissionInput, PreferencesUpdate, RuleInput, ScheduleInput};
use shorts_domain::{
    AccountId, PermissionStatus, Platform, PublicationId, PublicationState, RuleOutput,
};
use shorts_scheduler::Disposition;
use shorts_store::PublicationFilter;
use support::{Harness, harness};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "AQVtest-pasted-linkedin-access-token";
const ORG: &str = "urn:li:organization:4242";

struct World {
    h: Harness,
    server: MockServer,
    credentials: Arc<MemoryCredentials>,
    linkedin: Arc<LinkedInConnector>,
}

async fn world() -> World {
    let h = harness().await;
    let server = MockServer::start().await;
    let credentials = Arc::new(MemoryCredentials::default());
    let linkedin = LinkedInConnector::new(
        h.app.clone(),
        LinkedInSetup {
            api_base: server.uri().parse().unwrap(),
            api_version: "202609".into(),
            request_timeout: Duration::from_millis(500),
            part_timeout: Duration::from_millis(500),
            poll_every: Duration::from_millis(1),
        },
        credentials.clone(),
    );
    World {
        h,
        server,
        credentials,
        linkedin,
    }
}

impl World {
    async fn mock_page_name(&self) {
        Mock::given(method("GET"))
            .and(path("/rest/organizations/4242"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "localizedName": "Timeless Way Interactive"
            })))
            .mount(&self.server)
            .await;
    }

    async fn connect(&self) -> AccountId {
        self.mock_page_name().await;
        let view = self.linkedin.connect(TOKEN, "4242", None).await.unwrap();
        view.id.parse().unwrap()
    }

    async fn route_to(&self, account: AccountId) {
        let c = self.h.app.create_collection("To LinkedIn").await.unwrap();
        let id = c.id.parse().unwrap();
        self.h
            .app
            .create_rule(
                id,
                RuleInput {
                    name: "Page".into(),
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
        let file = self.h.video("devlog.mp4", &vec![7u8; 100 * 1024]);
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
        assert_eq!(pubs.len(), 1, "the rule made one LinkedIn publication");
        let p = &pubs[0];
        assert_eq!(p.platform(), Platform::Linkedin);
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
        let base = self.server.uri();
        Mock::given(method("POST"))
            .and(path("/rest/videos"))
            .and(query_param("action", "initializeUpload"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .respond_with(move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let size = body["initializeUploadRequest"]["fileSizeBytes"]
                    .as_u64()
                    .unwrap();
                assert_eq!(body["initializeUploadRequest"]["owner"], ORG);
                ResponseTemplate::new(200).set_body_json(json!({
                    "value": {
                        "video": "urn:li:video:V1",
                        "uploadToken": "",
                        "uploadInstructions": [
                            { "uploadUrl": format!("{base}/part/1"), "firstByte": 0, "lastByte": size - 1 }
                        ]
                    }
                }))
            })
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/part/1"))
            .respond_with(ResponseTemplate::new(201).insert_header("ETag", "e1"))
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/videos"))
            .and(query_param("action", "finalizeUpload"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/videos/urn%3Ali%3Avideo%3AV1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "status": "AVAILABLE" })),
            )
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/posts"))
            .respond_with(
                ResponseTemplate::new(201).insert_header("x-restli-id", "urn:li:share:77"),
            )
            .expect(1)
            .mount(&self.server)
            .await;
    }
}

#[tokio::test]
async fn a_watch_folder_video_goes_through_a_rule_and_approval_to_the_page() {
    let w = world().await;
    let before = w.linkedin.status().await.unwrap();
    assert!(before.pages.is_empty() && before.next_step.is_some());

    let account = w.connect().await;
    let stored = w.h.app.store().accounts().require(account).await.unwrap();
    assert_eq!(stored.display_name, "Timeless Way Interactive");
    assert_eq!(stored.credential_ref, Some(format!("linkedin:{account}")));
    assert!(stored.capabilities.can_publish_automatically());
    assert!(!serde_json::to_string(&stored).unwrap().contains(TOKEN));
    let secret = w
        .credentials
        .get(&format!("linkedin:{account}"))
        .unwrap()
        .unwrap();
    assert!(secret.contains(TOKEN) && secret.contains(ORG));
    let status = w.linkedin.status().await.unwrap();
    assert!(status.next_step.is_none());
    assert!(status.pages[0].connected && !status.pages[0].expires_soon);
    assert!(!serde_json::to_string(&status).unwrap().contains(TOKEN));

    w.route_to(account).await;
    w.mock_posting().await;
    let id = w.approved_upload().await;
    let ran = w.h.drain().await;
    assert_eq!(ran, vec![Disposition::Succeeded], "{ran:?}");
    let view = w.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert_eq!(
        view.published_url.as_deref(),
        Some("https://www.linkedin.com/feed/update/urn:li:share:77/")
    );
}

#[tokio::test]
async fn pages_are_found_from_the_token_or_entered_by_id() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path("/rest/organizationAcls"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "elements": [{ "organization": ORG, "role": "ADMINISTRATOR", "state": "APPROVED" }]
        })))
        .mount(&w.server)
        .await;
    w.mock_page_name().await;
    let pages = w.linkedin.find_pages(TOKEN).await.unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].urn, ORG);
    assert_eq!(pages[0].name, "Timeless Way Interactive");

    // Without the admin scope LinkedIn refuses the listing; the owner enters the id.
    Mock::given(method("GET"))
        .and(path("/rest/organizationAcls"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "message": "Not enough permissions" })),
        )
        .mount(&w.server)
        .await;
    let err = w
        .linkedin
        .find_pages("AQV-token-without-admin-scope-x")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("page's id"), "{err}");

    // A page whose name can't be read still connects, under a generic name.
    let view = w
        .linkedin
        .connect(TOKEN, "https://www.linkedin.com/company/99/admin/", None)
        .await
        .unwrap();
    assert_eq!(view.display_name, "LinkedIn page 99");

    let err = w
        .linkedin
        .connect(TOKEN, "not-a-page", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("numeric id"), "{err}");
}

#[tokio::test]
async fn a_bad_token_is_refused_when_connecting() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path("/rest/organizations/4242"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "message": "Invalid access token" })),
        )
        .mount(&w.server)
        .await;
    let err = w.linkedin.connect(TOKEN, "4242", None).await.unwrap_err();
    assert!(err.to_string().contains("token generator"), "{err}");
    let err = w.linkedin.connect("short", "4242", None).await.unwrap_err();
    assert!(err.to_string().contains("whole access token"), "{err}");
    assert!(w.linkedin.status().await.unwrap().pages.is_empty());
}

#[tokio::test]
async fn tokens_warn_before_running_out_and_reconnecting_replaces_them() {
    let w = world().await;
    let account = w.connect().await;
    w.h.clock.advance(chrono::Duration::days(55));
    let status = w.linkedin.status().await.unwrap();
    assert!(status.pages[0].expires_soon && !status.pages[0].needs_reconnect);

    w.h.clock.advance(chrono::Duration::days(6));
    let status = w.linkedin.status().await.unwrap();
    assert!(status.pages[0].needs_reconnect);
    assert!(status.pages[0].last_error.is_some());
    let view = w
        .linkedin
        .connect("AQV-second-pasted-linkedin-token", "4242", Some(account))
        .await
        .unwrap();
    assert_eq!(view.id, account.to_string());
    let status = w.linkedin.status().await.unwrap();
    assert!(!status.pages[0].needs_reconnect && !status.pages[0].expires_soon);
    assert!(
        w.credentials
            .get(&format!("linkedin:{account}"))
            .unwrap()
            .unwrap()
            .contains("AQV-second")
    );

    // Reconnecting a page with a different page's id is refused.
    Mock::given(method("GET"))
        .and(path("/rest/organizations/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "localizedName": "Other" })))
        .mount(&w.server)
        .await;
    let err = w
        .linkedin
        .connect(TOKEN, "5", Some(account))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("different page"), "{err}");

    let status = w.linkedin.disconnect(account).await.unwrap();
    assert!(!status.pages[0].connected);
}
