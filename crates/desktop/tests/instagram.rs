//! The Instagram connector end to end, against a wiremock fake of the Graph API
//! (`graph.facebook.com`, Facebook Login) and the upload host: connect through a Page with a
//! pasted user token, let a watch-folder video go through a workflow to a published reel, migrate
//! an account connected with Instagram login, and retry the reel that failed with "video_url is
//! required" once it is reconnected. Nothing here calls Meta.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shorts_desktop::NewWorkflow;
use shorts_desktop::credentials::{CredentialStore, MemoryCredentials};
use shorts_desktop::instagram::{InstagramConnector, InstagramSetup};
use shorts_domain::instagram::InstagramLogin;
use shorts_domain::workflow::WorkflowStatus;
use shorts_domain::{
    AccountId, NextAction, Platform, Publication, PublicationState, PublishAccess, UserAction,
    WorkflowSettings,
};
use shorts_scheduler::Disposition;
use shorts_store::PublicationFilter;
use support::{Harness, harness};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const USER_TOKEN: &str = "EAAtest-long-lived-user-token";
const PAGE_TOKEN: &str = "EAAtest-page-token-from-me-accounts";
const IG_USER: &str = "17841400000000001";
const ALL_SCOPES: &[&str] = &[
    "instagram_basic",
    "instagram_content_publish",
    "pages_show_list",
    "pages_read_engagement",
    "business_management",
];

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
            instagram_login_base: format!("{base}/ig").parse().unwrap(),
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

fn page(id: &str, name: &str, token: &str, ig: Option<(&str, &str)>) -> serde_json::Value {
    let mut p = json!({ "id": id, "name": name, "access_token": token });
    if let Some((ig_id, username)) = ig {
        p["instagram_business_account"] = json!({ "id": ig_id, "username": username });
    }
    p
}

impl World {
    /// `/debug_token` for `token`: these scopes, expiring in `expires_in` seconds (0 = never).
    async fn mock_debug(&self, token: &str, scopes: &[&str], expires_in: i64) {
        let expires_at = if expires_in == 0 {
            0
        } else {
            support::t0().timestamp() + expires_in
        };
        Mock::given(method("GET"))
            .and(path("/v24.0/debug_token"))
            .and(query_param("input_token", token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "app_id": "1", "type": "USER", "is_valid": true,
                          "expires_at": expires_at, "scopes": scopes }
            })))
            .mount(&self.server)
            .await;
    }

    async fn mock_pages(&self, token: &str, pages: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/v24.0/me/accounts"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": pages })))
            .mount(&self.server)
            .await;
    }

    /// A long-lived user token that sees one Page with a linked Instagram account.
    async fn mock_one_page(&self) {
        self.mock_debug(USER_TOKEN, ALL_SCOPES, 60 * 24 * 3600)
            .await;
        self.mock_pages(
            USER_TOKEN,
            json!([page(
                "p1",
                "Timeless Way Interactive",
                PAGE_TOKEN,
                Some((IG_USER, "testkitchen"))
            )]),
        )
        .await;
    }

    async fn connect(&self) -> AccountId {
        self.mock_one_page().await;
        let view = self
            .instagram
            .connect(USER_TOKEN, None, None)
            .await
            .unwrap();
        view.id.parse().unwrap()
    }

    async fn workflow(&self, account: AccountId) {
        self.workflow_with(account, true).await;
    }

    async fn workflow_with(&self, account: AccountId, auto_publish: bool) {
        self.h
            .app
            .create_workflow(NewWorkflow {
                account_id: account,
                name: None,
                auto_publish,
                rights_confirmed: true,
                effective_from: None,
                settings: WorkflowSettings::default(),
                caption_template: None,
                hashtags: vec![],
            })
            .await
            .unwrap();
    }

    async fn publications(&self) -> Vec<Publication> {
        self.h
            .app
            .store()
            .publications()
            .list(&PublicationFilter::default())
            .await
            .unwrap()
    }

    /// The upload, processing and publish calls for container `c1`, all with the Page token.
    async fn mock_publishing(&self) {
        Mock::given(method("POST"))
            .and(path(format!("/v24.0/{IG_USER}/media")))
            .and(header(
                "authorization",
                format!("Bearer {PAGE_TOKEN}").as_str(),
            ))
            .and(body_string_contains("media_type=REELS"))
            .and(body_string_contains("upload_type=resumable"))
            .and(body_string_contains("share_to_feed=true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "uri": format!("{}/ig-api-upload/v24.0/c1", self.server.uri()),
            })))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v24.0/c1"))
            .and(query_param("fields", "video_status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "video_status": { "uploading_phase": { "status": "not_started", "bytes_transferred": 0 } }
            })))
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ig-api-upload/v24.0/c1"))
            .and(header(
                "authorization",
                format!("OAuth {PAGE_TOKEN}").as_str(),
            ))
            .and(header("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
            .expect(1)
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v24.0/c1"))
            .and(query_param("fields", "status_code,status"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "status_code": "FINISHED" })),
            )
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v24.0/{IG_USER}/content_publishing_limit")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "quota_usage": 0, "config": { "quota_total": 100 } }]
            })))
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v24.0/{IG_USER}/media_publish")))
            .and(body_string_contains("creation_id=c1"))
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
async fn a_watch_folder_video_goes_through_a_workflow_to_a_published_reel() {
    let w = world().await;
    let before = w.instagram.status().await.unwrap();
    assert!(before.accounts.is_empty());
    assert!(before.next_step.is_some());

    // One Page with a linked account: picked without asking.
    let account = w.connect().await;
    let stored = w.h.app.store().accounts().require(account).await.unwrap();
    assert_eq!(stored.display_name, "@testkitchen");
    assert_eq!(stored.credential_ref, Some(format!("instagram:{account}")));
    assert!(stored.capabilities.can_publish_automatically());
    // Secrets stay in the credential store; the database only names the entry.
    let raw = serde_json::to_string(&stored).unwrap();
    assert!(!raw.contains(PAGE_TOKEN) && !raw.contains(IG_USER) && !raw.contains(USER_TOKEN));
    let secret = w
        .credentials
        .get(&format!("instagram:{account}"))
        .unwrap()
        .unwrap();
    assert!(secret.contains(PAGE_TOKEN) && secret.contains(IG_USER));
    assert!(!secret.contains(USER_TOKEN), "only the Page token is kept");
    let status = w.instagram.status().await.unwrap();
    assert!(status.next_step.is_none());
    let a = &status.accounts[0];
    assert!(a.connected && a.can_publish && !a.needs_reconnect);
    assert_eq!(a.login, Some(InstagramLogin::FacebookLogin));
    assert_eq!(a.page_name.as_deref(), Some("Timeless Way Interactive"));
    assert_eq!(a.username.as_deref(), Some("testkitchen"));
    let shown = serde_json::to_string(&status).unwrap();
    assert!(!shown.contains(PAGE_TOKEN) && !shown.contains(IG_USER));

    w.workflow(account).await;
    w.mock_publishing().await;
    let file = w.h.video("bagels.mp4", &vec![7u8; 100 * 1024]);
    w.h.app.import_watched(&file).await.unwrap();
    let ran = w.h.drain().await;
    assert!(ran.contains(&Disposition::Succeeded), "{ran:?}");
    let p = w.publications().await.remove(0);
    assert_eq!(p.platform(), Platform::InstagramReels);
    let view = w.h.app.publication(p.id()).await.unwrap();
    assert_eq!(view.state, PublicationState::Published, "{view:?}");
    assert_eq!(
        view.published_url.as_deref(),
        Some("https://www.instagram.com/reel/AbC123/")
    );
    assert_eq!(
        view.approved_by.as_deref(),
        Some("workflow:TikTok → Instagram Reels")
    );
}

#[tokio::test]
async fn pages_are_listed_and_the_owner_picks_one_when_there_are_several() {
    let w = world().await;
    w.mock_debug(USER_TOKEN, ALL_SCOPES, 0).await;
    w.mock_pages(
        USER_TOKEN,
        json!([
            page("p0", "Old blog", "EAA-old-page-token", None),
            page(
                "p1",
                "Timeless Way Interactive",
                PAGE_TOKEN,
                Some((IG_USER, "testkitchen"))
            ),
            page(
                "p2",
                "Second studio",
                "EAA-second-page-token",
                Some(("17841400000000002", "studio2"))
            ),
        ]),
    )
    .await;
    let found = w.instagram.find_pages(USER_TOKEN).await.unwrap();
    assert!(found.warning.is_none());
    let names: Vec<_> = found.pages.iter().map(|p| p.page_name.as_str()).collect();
    assert_eq!(
        names,
        ["Timeless Way Interactive", "Second studio", "Old blog"],
        "linked Pages first"
    );
    assert_eq!(found.pages[2].instagram_username, None);
    assert!(!serde_json::to_string(&found).unwrap().contains("EAA-"));

    // Two linked Pages: no guessing.
    let err = w
        .instagram
        .connect(USER_TOKEN, None, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Choose which Page to connect: Timeless Way Interactive and Second studio"),
        "{err}"
    );
    // A Page without an Instagram account says how to link one.
    let err = w
        .instagram
        .connect(USER_TOKEN, Some("p0"), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Linked accounts"), "{err}");

    let view = w
        .instagram
        .connect(USER_TOKEN, Some("p2"), None)
        .await
        .unwrap();
    assert_eq!(view.display_name, "@studio2");
    let secret = w
        .credentials
        .get(&format!("instagram:{}", view.id))
        .unwrap()
        .unwrap();
    assert!(secret.contains("EAA-second-page-token"));
}

#[tokio::test]
async fn every_refusal_says_what_to_do_next() {
    let w = world().await;
    let err = |e: shorts_desktop::AppError| e.to_string();

    // No Pages at all.
    w.mock_debug("EAA-no-pages-token-value", ALL_SCOPES[..4].as_ref(), 0)
        .await;
    w.mock_pages("EAA-no-pages-token-value", json!([])).await;
    let e = err(w
        .instagram
        .find_pages("EAA-no-pages-token-value")
        .await
        .unwrap_err());
    assert!(e.contains("can't see any Facebook Pages"), "{e}");
    assert!(e.contains("business_management"), "{e}");

    // Pages, but none with a linked Instagram account.
    w.mock_debug("EAA-unlinked-token-value", ALL_SCOPES, 0)
        .await;
    w.mock_pages(
        "EAA-unlinked-token-value",
        json!([page("p1", "Timeless Way Interactive", PAGE_TOKEN, None)]),
    )
    .await;
    let e = err(w
        .instagram
        .connect("EAA-unlinked-token-value", None, None)
        .await
        .unwrap_err());
    assert!(
        e.contains("Timeless Way Interactive has no linked Instagram account")
            && e.contains("Page's settings → Linked accounts"),
        "{e}"
    );

    // Missing permissions are named.
    w.mock_debug(
        "EAA-few-scopes-token-value",
        &["instagram_basic", "pages_show_list"],
        0,
    )
    .await;
    let e = err(w
        .instagram
        .find_pages("EAA-few-scopes-token-value")
        .await
        .unwrap_err());
    assert!(
        e.contains("missing the instagram_content_publish and pages_read_engagement permissions"),
        "{e}"
    );

    // A short-lived token is sent to the Access Token Debugger.
    w.mock_debug("EAA-short-lived-token-value", ALL_SCOPES, 3600)
        .await;
    let e = err(w
        .instagram
        .find_pages("EAA-short-lived-token-value")
        .await
        .unwrap_err());
    assert!(
        e.contains("short-lived") && e.contains("Extend Access Token"),
        "{e}"
    );

    // An invalid token.
    Mock::given(method("GET"))
        .and(path("/v24.0/debug_token"))
        .and(query_param("input_token", "EAA-revoked-token-value"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "Error validating access token", "code": 190, "error_subcode": 460 }
        })))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v24.0/me/permissions"))
        .and(header("authorization", "Bearer EAA-revoked-token-value"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "Error validating access token", "code": 190, "error_subcode": 460 }
        })))
        .mount(&w.server)
        .await;
    let e = err(w
        .instagram
        .find_pages("EAA-revoked-token-value")
        .await
        .unwrap_err());
    assert!(
        e.contains("password") && e.contains("Graph API Explorer"),
        "{e}"
    );

    // An Instagram login token pasted into the Facebook login form.
    let e = err(w
        .instagram
        .find_pages("IGAA-instagram-login-token-value")
        .await
        .unwrap_err());
    assert!(e.contains("Instagram login token"), "{e}");
    let e = err(w.instagram.find_pages("short").await.unwrap_err());
    assert!(e.contains("whole access token"), "{e}");

    // When debug_token isn't available, permissions come from /me/permissions and the
    // owner is told how to check the lifetime.
    Mock::given(method("GET"))
        .and(path("/v24.0/debug_token"))
        .and(query_param("input_token", "EAA-no-debug-token-value"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "(#100) You must provide an app access token", "code": 100 }
        })))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v24.0/me/permissions"))
        .and(header("authorization", "Bearer EAA-no-debug-token-value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": ALL_SCOPES.iter().map(|p| json!({ "permission": p, "status": "granted" })).collect::<Vec<_>>()
        })))
        .mount(&w.server)
        .await;
    w.mock_pages(
        "EAA-no-debug-token-value",
        json!([page(
            "p1",
            "Timeless Way Interactive",
            PAGE_TOKEN,
            Some((IG_USER, "testkitchen"))
        )]),
    )
    .await;
    let found = w
        .instagram
        .find_pages("EAA-no-debug-token-value")
        .await
        .unwrap();
    assert!(
        found
            .warning
            .as_deref()
            .unwrap()
            .contains("Access Token Debugger")
    );
    assert!(w.instagram.status().await.unwrap().accounts.is_empty());
}

#[tokio::test]
async fn a_revoked_page_token_asks_for_a_reconnect_and_reconnecting_resumes() {
    let w = world().await;
    let account = w.connect().await;
    w.workflow(account).await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{IG_USER}/media")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "Error validating access token: The session has been invalidated because the user changed their password",
                       "code": 190, "error_subcode": 460 }
        })))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&w.server)
        .await;
    let file = w.h.video("bagels.mp4", &vec![7u8; 100 * 1024]);
    w.h.app.import_watched(&file).await.unwrap();
    w.h.drain().await;
    let p = w.publications().await.remove(0);
    assert_eq!(p.state(), PublicationState::NeedsAuthentication);
    assert!(p.last_error().unwrap().contains("password"), "{p:?}");
    let status = w.instagram.status().await.unwrap();
    assert!(status.accounts[0].needs_reconnect);
    let detail = w.h.app.list_workflows().await.unwrap();
    assert_eq!(detail.workflows[0].status, WorkflowStatus::NeedsReconnect);

    // A new token for the same account reconnects it and the reel goes out.
    w.mock_publishing().await;
    w.instagram
        .connect(USER_TOKEN, None, Some(account))
        .await
        .unwrap();
    assert!(!w.instagram.status().await.unwrap().accounts[0].needs_reconnect);
    w.h.drain().await;
    let p = w.h.app.publication(p.id()).await.unwrap();
    assert_eq!(p.state, PublicationState::Published, "{p:?}");

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

/// What the app stored for an account connected with Instagram login before this change.
fn legacy_grant() -> String {
    json!({
        "access_token": "IGAA-old-instagram-login-token",
        "user_id": IG_USER,
        "username": "testkitchen",
        "issued_at": "2026-10-20T12:00:00Z",
        "expires_at": "2026-12-19T12:00:00Z",
    })
    .to_string()
}

#[tokio::test]
async fn the_reel_stuck_on_video_url_publishes_on_retry_after_reconnecting() {
    let w = world().await;
    // The owner's first real test: the container was refused for want of a video_url.
    let account = w.connect().await;
    w.workflow(account).await;
    Mock::given(method("POST"))
        .and(path(format!("/v24.0/{IG_USER}/media")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "The parameter video_url is required", "type": "OAuthException",
                       "code": 100, "fbtrace_id": "x" }
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&w.server)
        .await;
    let file = w.h.video("bagels.mp4", &vec![7u8; 100 * 1024]);
    w.h.app.import_watched(&file).await.unwrap();
    w.h.drain().await;
    let stuck = w.publications().await.remove(0);
    assert_eq!(stuck.state(), PublicationState::NeedsUserAction);
    assert!(
        matches!(stuck.user_action(), Some(UserAction::FixValidation { issues }) if issues[0].contains("video_url")),
        "{stuck:?}"
    );

    // ...and that account was connected with Instagram login, as the old app stored it.
    w.credentials
        .set(&format!("instagram:{account}"), &legacy_grant())
        .unwrap();

    // On start the account moves to export packages and asks for Facebook login. The
    // destination, its workflow and the stuck reel stay.
    assert_eq!(w.instagram.migrate().await.unwrap(), 1);
    assert_eq!(w.instagram.migrate().await.unwrap(), 0, "once");
    let stored = w.h.app.store().accounts().require(account).await.unwrap();
    assert!(matches!(
        &stored.capabilities.publish_access,
        PublishAccess::ManualOnly { missing }
            if missing.next_action == NextAction::Reconnect && missing.reason.contains("Facebook login")
    ));
    let status = w.instagram.status().await.unwrap();
    let a = &status.accounts[0];
    assert!(a.connected && a.needs_reconnect && !a.can_publish);
    assert_eq!(a.login, Some(InstagramLogin::InstagramLogin));
    assert!(
        a.last_error
            .as_deref()
            .unwrap()
            .contains("can't upload files")
    );
    assert!(status.next_step.is_some());
    let workflows = w.h.app.list_workflows().await.unwrap();
    assert_eq!(
        workflows.workflows[0].status,
        WorkflowStatus::NeedsReconnect
    );

    // Reconnecting with Facebook login keeps the same destination (matched by account).
    w.mock_publishing().await;
    let view = w.instagram.connect(USER_TOKEN, None, None).await.unwrap();
    assert_eq!(view.id, account.to_string());
    assert!(view.automatic);
    let a = &w.instagram.status().await.unwrap().accounts[0];
    assert!(a.can_publish && !a.needs_reconnect);
    assert_eq!(a.page_name.as_deref(), Some("Timeless Way Interactive"));
    let workflows = w.h.app.list_workflows().await.unwrap();
    assert_eq!(workflows.workflows[0].status, WorkflowStatus::Active);
    assert!(workflows.workflows[0].auto_publish, "the grant stayed");

    // Retry (the "Needs you" row's button) publishes it.
    let view = w.h.app.publication(stuck.id()).await.unwrap();
    assert!(
        view.actions
            .contains(&shorts_desktop::views::Action::ResolveUserAction),
        "{:?}",
        view.actions
    );
    w.h.app.resolve_user_action(stuck.id()).await.unwrap();
    let ran = w.h.drain().await;
    assert!(ran.contains(&Disposition::Succeeded), "{ran:?}");
    let view = w.h.app.publication(stuck.id()).await.unwrap();
    assert_eq!(view.state, PublicationState::Published, "{view:?}");
    assert_eq!(
        view.published_url.as_deref(),
        Some("https://www.instagram.com/reel/AbC123/")
    );
}

#[tokio::test]
async fn instagram_login_stays_as_an_export_only_fallback() {
    let w = world().await;
    Mock::given(method("GET"))
        .and(path("/ig/v24.0/me"))
        .and(header(
            "authorization",
            "Bearer IGAA-pasted-instagram-login",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "app-scoped", "user_id": IG_USER, "username": "testkitchen",
            "account_type": "MEDIA_CREATOR"
        })))
        .mount(&w.server)
        .await;
    let view = w
        .instagram
        .connect_instagram_login("IGAA-pasted-instagram-login", None)
        .await
        .unwrap();
    assert!(!view.automatic);
    assert_eq!(view.next_action, Some(NextAction::Reconnect));
    assert!(
        view.missing_reason
            .as_deref()
            .unwrap()
            .contains("export packages")
    );
    let account: AccountId = view.id.parse().unwrap();

    // Its videos become export packages.
    w.workflow_with(account, false).await;
    let file = w.h.video("bagels.mp4", &vec![7u8; 100 * 1024]);
    w.h.app.import_watched(&file).await.unwrap();
    w.h.drain().await;
    let id = w.publications().await.remove(0).id();
    w.h.app.publish_now(id).await.unwrap();
    w.h.drain().await;
    let p = w.h.app.publication(id).await.unwrap();
    assert_eq!(p.state, PublicationState::NeedsUserAction, "{p:?}");
    assert!(
        p.actions
            .contains(&shorts_desktop::views::Action::ConfirmPosted)
    );

    // Personal accounts are refused with the next step.
    Mock::given(method("GET"))
        .and(path("/ig/v24.0/me"))
        .and(header(
            "authorization",
            "Bearer IGAA-personal-account-token",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user_id": "1", "username": "me", "account_type": "PERSONAL"
        })))
        .mount(&w.server)
        .await;
    let err = w
        .instagram
        .connect_instagram_login("IGAA-personal-account-token", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("professional"), "{err}");
}
