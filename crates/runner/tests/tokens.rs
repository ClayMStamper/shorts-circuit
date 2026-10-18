//! Token refresh on the runner, against fake OAuth and Graph servers.

mod support;

use chrono::Duration;
use shorts_adapters::AdapterError;
use shorts_adapters::instagram::InstagramAuth;
use shorts_adapters::linkedin::LinkedInAuth;
use shorts_adapters::snapchat::SnapAuth;
use shorts_adapters::youtube::AccessTokens;
use shorts_cloud::{AccountPut, CloudCredential, CredentialSource, Secret};
use shorts_domain::{AccountId, Platform};
use shorts_runner::RunnerSetup;
use support::{Fixture, t0, youtube_credential};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn fixture(server: &MockServer) -> Option<Fixture> {
    let base = |p: &str| reqwest::Url::parse(&format!("{}{p}", server.uri())).unwrap();
    let mut setup = RunnerSetup::default();
    setup.google.token = base("/google/token");
    setup.snap_oauth.token = base("/snap/token");
    setup.instagram.graph_base = base("/ig");
    Fixture::with_setup(setup).await
}

async fn add_account(f: &Fixture, platform: Platform, credential: CloudCredential) -> AccountId {
    let account = support::automatic(platform, "Another account");
    f.store()
        .put_account(
            f.installation.id,
            &AccountPut {
                account: account.clone(),
                credential,
                base_revision: None,
            },
            t0(),
        )
        .await
        .unwrap();
    account.id
}

#[tokio::test]
async fn google_access_tokens_are_refreshed_cached_and_rotations_kept() {
    let server = MockServer::start().await;
    let Some(f) = fixture(&server).await else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/google/token"))
        .and(body_string_contains("refresh_token=fake-refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-access-1",
            "expires_in": 3600,
            "refresh_token": "fake-refresh-rotated",
            "scope": "https://www.googleapis.com/auth/youtube.upload",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = f.runner.tokens();
    let account = f.account.id;
    assert_eq!(tokens.access_token(account).await.unwrap(), "fake-access-1");
    // Cached: no second refresh.
    assert_eq!(tokens.access_token(account).await.unwrap(), "fake-access-1");
    let stored = f.store().credential(account).await.unwrap().unwrap();
    assert_eq!(stored.revision, 2);
    assert_eq!(stored.updated_by, CredentialSource::Runner);
    assert_eq!(
        stored.credential,
        youtube_credential("fake-refresh-rotated")
    );
    f.finish().await;
}

#[tokio::test]
async fn a_refused_google_refresh_asks_for_a_reconnect() {
    let server = MockServer::start().await;
    let Some(f) = fixture(&server).await else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/google/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "Token has been expired or revoked.",
        })))
        .mount(&server)
        .await;
    let err = f
        .runner
        .tokens()
        .access_token(f.account.id)
        .await
        .unwrap_err();
    let AdapterError::Authentication { message } = err else {
        panic!("{err:?}")
    };
    assert!(message.contains("reconnect"), "{message}");
    f.finish().await;
}

#[tokio::test]
async fn snap_refresh_tokens_rotate_and_are_kept() {
    let server = MockServer::start().await;
    let Some(f) = fixture(&server).await else {
        return;
    };
    let account = add_account(
        &f,
        Platform::SnapchatSpotlight,
        CloudCredential::SnapchatSpotlight {
            client_id: "fake-snap-client".into(),
            client_secret: Secret::new("fake-snap-secret"),
            refresh_token: Secret::new("fake-snap-refresh-1"),
            profile_id: "fake-profile".into(),
        },
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/snap/token"))
        .and(body_string_contains("refresh_token=fake-snap-refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-snap-access",
            "refresh_token": "fake-snap-refresh-2",
            "expires_in": 1800,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let session = SnapAuth::session(f.runner.tokens().as_ref(), account)
        .await
        .unwrap();
    assert_eq!(session.access_token, "fake-snap-access");
    assert_eq!(session.profile_id, "fake-profile");
    let stored = f.store().credential(account).await.unwrap().unwrap();
    assert_eq!(stored.updated_by, CredentialSource::Runner);
    let CloudCredential::SnapchatSpotlight { refresh_token, .. } = stored.credential else {
        panic!()
    };
    assert_eq!(refresh_token.expose(), "fake-snap-refresh-2");
    f.finish().await;
}

#[tokio::test]
async fn instagram_tokens_are_renewed_when_under_20_days_are_left() {
    let server = MockServer::start().await;
    let Some(f) = fixture(&server).await else {
        return;
    };
    let fresh = add_account(
        &f,
        Platform::InstagramReels,
        CloudCredential::InstagramReels {
            access_token: Secret::new("fake-ig-fresh"),
            user_id: "fake-ig-user".into(),
            issued_at: t0() - Duration::days(1),
            expires_at: t0() + Duration::days(59),
        },
    )
    .await;
    let due = add_account(
        &f,
        Platform::InstagramReels,
        CloudCredential::InstagramReels {
            access_token: Secret::new("fake-ig-old"),
            user_id: "fake-ig-user-2".into(),
            issued_at: t0() - Duration::days(45),
            expires_at: t0() + Duration::days(15),
        },
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/ig/refresh_access_token"))
        .and(query_param("access_token", "fake-ig-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-ig-renewed",
            "token_type": "bearer",
            "expires_in": 5_184_000,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = f.runner.tokens().as_ref();
    let s = InstagramAuth::session(tokens, fresh).await.unwrap();
    assert_eq!(s.access_token, "fake-ig-fresh");
    let s = InstagramAuth::session(tokens, due).await.unwrap();
    assert_eq!(s.access_token, "fake-ig-renewed");
    let stored = f.store().credential(due).await.unwrap().unwrap();
    let CloudCredential::InstagramReels { expires_at, .. } = stored.credential else {
        panic!()
    };
    assert_eq!(expires_at, t0() + Duration::days(60));
    // Renewed now, so it isn't renewed again.
    assert_eq!(
        InstagramAuth::session(tokens, due)
            .await
            .unwrap()
            .access_token,
        "fake-ig-renewed"
    );
    f.finish().await;
}

#[tokio::test]
async fn linkedin_tokens_are_used_until_they_run_out() {
    let server = MockServer::start().await;
    let Some(f) = fixture(&server).await else {
        return;
    };
    let account = add_account(
        &f,
        Platform::Linkedin,
        CloudCredential::Linkedin {
            access_token: Secret::new("fake-li-token"),
            organization: "urn:li:organization:123".into(),
            expires_at: t0() + Duration::days(1),
        },
    )
    .await;
    let tokens = f.runner.tokens().as_ref();
    let s = LinkedInAuth::session(tokens, account).await.unwrap();
    assert_eq!(s.author, "urn:li:organization:123");
    f.clock.advance(Duration::days(2));
    assert!(matches!(
        LinkedInAuth::session(tokens, account).await,
        Err(AdapterError::Authentication { .. })
    ));
    f.finish().await;
}

#[tokio::test]
async fn credentials_survive_rotating_the_master_key() {
    let Some(db) = support::TestDb::new().await else {
        return;
    };
    let old_key = shorts_runner::Vault::generate_key();
    let old = db
        .reopen(shorts_runner::Vault::new(&old_key, &[]).unwrap())
        .await;
    let (inst, _) = old.create_installation("PC", t0()).await.unwrap();
    let account = support::automatic(Platform::YoutubeShorts, "Channel");
    old.put_account(
        inst.id,
        &AccountPut {
            account: account.clone(),
            credential: youtube_credential("fake-refresh-keep"),
            base_revision: None,
        },
        t0(),
    )
    .await
    .unwrap();

    let new_key = shorts_runner::Vault::generate_key();
    let rotating = db
        .reopen(shorts_runner::Vault::new(&new_key, &[old_key.as_str()]).unwrap())
        .await;
    assert_eq!(rotating.rewrap_credentials().await.unwrap(), 1);
    assert_eq!(
        rotating.credential_key_ids().await.unwrap(),
        vec![rotating.vault().active_key_id().to_owned()]
    );
    assert_eq!(rotating.rewrap_credentials().await.unwrap(), 0);

    let rotated = db
        .reopen(shorts_runner::Vault::new(&new_key, &[]).unwrap())
        .await;
    assert_eq!(
        rotated
            .credential(account.id)
            .await
            .unwrap()
            .unwrap()
            .credential,
        youtube_credential("fake-refresh-keep")
    );
    // The wrong key can't read it.
    let wrong = db
        .reopen(shorts_runner::Vault::new(&shorts_runner::Vault::generate_key(), &[]).unwrap())
        .await;
    assert!(wrong.credential(account.id).await.is_err());
    db.drop().await;
}
