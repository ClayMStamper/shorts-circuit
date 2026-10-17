//! The OAuth broker against a fake TikTok token endpoint (wiremock). No real credentials.

use reqwest::{StatusCode, Url};
use serde_json::{Value, json};
use shorts_adapters::broker::{BrokerClient, Claim, challenge_for};
use shorts_backend::{BrokerConfig, TikTokApp, router};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const STATE: &str = "state-0123456789abcdefghijkl";
const VERIFIER: &str = "verifier-0123456789abcdefghijklmnopqrstuvwxyz";

async fn serve(config: BrokerConfig) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(config, reqwest::Client::new()))
            .await
            .unwrap();
    });
    Url::parse(&format!("http://{addr}/")).unwrap()
}

fn tiktok_app(fake: &MockServer) -> TikTokApp {
    TikTokApp {
        client_key: "test-client-key".into(),
        client_secret: "test-client-secret".into(),
        redirect_uri: Url::parse("https://broker.example/v1/tiktok/callback").unwrap(),
        authorize_url: Url::parse(&format!("{}/v2/auth/authorize/", fake.uri())).unwrap(),
        token_url: Url::parse(&format!("{}/v2/oauth/token/", fake.uri())).unwrap(),
        revoke_url: Url::parse(&format!("{}/v2/oauth/revoke/", fake.uri())).unwrap(),
    }
}

fn tokens(access: &str) -> Value {
    json!({
        "access_token": access,
        "expires_in": 86400,
        "open_id": "test-open-id",
        "refresh_expires_in": 31536000,
        "refresh_token": "test-refresh-token",
        "scope": "user.info.basic,video.list",
        "token_type": "Bearer"
    })
}

fn no_redirects() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

#[tokio::test]
async fn without_a_tiktok_app_the_broker_says_so() {
    let base = serve(BrokerConfig::default()).await;
    let client = BrokerClient::new(reqwest::Client::new(), base.clone());
    assert!(!client.status().await.unwrap().configured);
    let response = no_redirects()
        .get(client.connect_url(STATE, &challenge_for(VERIFIER)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_desktop_collects_tokens_after_the_browser_flow() {
    let fake = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/oauth/token/"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=test-auth-code"))
        .and(body_string_contains("client_secret=test-client-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens("test-access-token")))
        .expect(1)
        .mount(&fake)
        .await;
    let base = serve(BrokerConfig {
        tiktok: Some(tiktok_app(&fake)),
    })
    .await;
    let client = BrokerClient::new(reqwest::Client::new(), base.clone());
    assert!(client.status().await.unwrap().configured);

    // The browser is sent to TikTok with the app's key, the read-only scopes and the state.
    let response = no_redirects()
        .get(client.connect_url(STATE, &challenge_for(VERIFIER)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert_eq!(query["client_key"], "test-client-key");
    assert_eq!(query["scope"], "user.info.basic,video.list");
    assert_eq!(query["state"], STATE);
    assert!(!location.as_str().contains("secret"));

    assert_eq!(client.claim(STATE, VERIFIER).await.unwrap(), Claim::Pending);

    // TikTok redirects back with a code; the broker exchanges it.
    let callback = base
        .join(&format!(
            "v1/tiktok/callback?code=test-auth-code&state={STATE}"
        ))
        .unwrap();
    let page = reqwest::get(callback).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(page.text().await.unwrap().contains("TikTok connected"));

    match client.claim(STATE, VERIFIER).await.unwrap() {
        Claim::Ready(t) => {
            assert_eq!(t.access_token, "test-access-token");
            assert_eq!(t.open_id, "test-open-id");
        }
        other => panic!("{other:?}"),
    }
    // Tokens are handed out once.
    assert!(matches!(
        client.claim(STATE, VERIFIER).await.unwrap(),
        Claim::Failed { .. }
    ));
}

#[tokio::test]
async fn a_wrong_verifier_ends_the_flow() {
    let fake = MockServer::start().await;
    let base = serve(BrokerConfig {
        tiktok: Some(tiktok_app(&fake)),
    })
    .await;
    let client = BrokerClient::new(reqwest::Client::new(), base);
    no_redirects()
        .get(client.connect_url(STATE, &challenge_for(VERIFIER)))
        .send()
        .await
        .unwrap();
    assert!(matches!(
        client
            .claim(STATE, "someone-else-guessing-0123456789")
            .await
            .unwrap(),
        Claim::Failed { .. }
    ));
    assert!(matches!(
        client.claim(STATE, VERIFIER).await.unwrap(),
        Claim::Failed { .. }
    ));
}

#[tokio::test]
async fn a_denied_authorization_is_reported_to_the_desktop() {
    let fake = MockServer::start().await;
    let base = serve(BrokerConfig {
        tiktok: Some(tiktok_app(&fake)),
    })
    .await;
    let client = BrokerClient::new(reqwest::Client::new(), base.clone());
    no_redirects()
        .get(client.connect_url(STATE, &challenge_for(VERIFIER)))
        .send()
        .await
        .unwrap();
    let callback = base
        .join(&format!(
            "v1/tiktok/callback?error=access_denied&error_description=User+cancelled&state={STATE}"
        ))
        .unwrap();
    reqwest::get(callback).await.unwrap();
    assert_eq!(
        client.claim(STATE, VERIFIER).await.unwrap(),
        Claim::Failed {
            message: "User cancelled".into()
        }
    );
}

#[tokio::test]
async fn refresh_uses_the_client_secret_and_reports_revoked_grants() {
    let fake = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/oauth/token/"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=good-refresh"))
        .and(body_string_contains("client_secret=test-client-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens("fresh-access")))
        .mount(&fake)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/oauth/token/"))
        .and(body_string_contains("refresh_token=revoked-refresh"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Refresh token is invalid or expired.",
            "log_id": "test"
        })))
        .mount(&fake)
        .await;
    let base = serve(BrokerConfig {
        tiktok: Some(tiktok_app(&fake)),
    })
    .await;
    let client = BrokerClient::new(reqwest::Client::new(), base);
    assert_eq!(
        client.refresh("good-refresh").await.unwrap().access_token,
        "fresh-access"
    );
    let err = client.refresh("revoked-refresh").await.unwrap_err();
    assert!(
        matches!(
            err,
            shorts_adapters::tiktok::IntakeError::Unauthorized { .. }
        ),
        "{err:?}"
    );
}
