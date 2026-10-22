//! Google's OAuth token endpoint, faked with wiremock. Nothing here calls Google.

use serde_json::json;
use shorts_adapters::google::{ClientConfig, GoogleEndpoints, OAuthClient, OAuthError};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client() -> ClientConfig {
    ClientConfig::new("123-test.apps.googleusercontent.com", "GOCSPX-test-secret").unwrap()
}

async fn oauth(server: &MockServer) -> OAuthClient {
    let base = server.uri();
    OAuthClient::new(
        reqwest::Client::new(),
        GoogleEndpoints {
            auth: format!("{base}/o/oauth2/v2/auth").parse().unwrap(),
            token: format!("{base}/token").parse().unwrap(),
            revoke: format!("{base}/revoke").parse().unwrap(),
        },
    )
}

#[tokio::test]
async fn code_exchange_sends_the_verifier_and_returns_a_refresh_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier=the-verifier"))
        .and(body_string_contains("code=4%2Fcode"))
        .and(body_string_contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.access",
            "expires_in": 3599,
            "refresh_token": "1//refresh",
            "refresh_token_expires_in": 604799,
            "scope": "https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly",
            "token_type": "Bearer"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = oauth(&server)
        .await
        .exchange_code(&client(), "4/code", "the-verifier", "http://127.0.0.1:5000")
        .await
        .unwrap();
    assert_eq!(tokens.refresh_token.as_deref(), Some("1//refresh"));
    assert_eq!(tokens.refresh_token_expires_in, Some(604_799));
    assert!(tokens.missing_scopes().is_empty());
}

#[tokio::test]
async fn an_expired_or_revoked_refresh_token_asks_for_a_new_sign_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Token has been expired or revoked."
        })))
        .mount(&server)
        .await;
    let err = oauth(&server)
        .await
        .refresh(&client(), "1//refresh")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        OAuthError::Reauthorize {
            message: "Token has been expired or revoked.".into()
        }
    );
}

#[tokio::test]
async fn a_wrong_client_secret_is_reported_as_such() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "Unauthorized"
        })))
        .mount(&server)
        .await;
    let err = oauth(&server)
        .await
        .refresh(&client(), "1//refresh")
        .await
        .unwrap_err();
    assert!(matches!(err, OAuthError::BadClient { .. }));
    assert!(!err.to_string().contains("GOCSPX"));
}

#[tokio::test]
async fn revoke_posts_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=1%2F%2Frefresh"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    oauth(&server).await.revoke("1//refresh").await.unwrap();
}
