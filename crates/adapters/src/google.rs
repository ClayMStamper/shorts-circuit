//! Google OAuth 2.0 for installed (desktop) apps: browser sign-in with a loopback redirect,
//! PKCE (S256) and a `state` check, then code exchange, refresh and revoke against Google's
//! token endpoints.
//!
//! A desktop client's secret is not confidential (Google says so for installed apps), so the
//! desktop app talks to Google directly instead of going through the OAuth broker. The owner
//! enters the client ID and secret; the desktop keeps them, and the refresh token, in the OS
//! credential store. Nothing here logs tokens, codes or secrets: the `Debug` impls redact them
//! and error messages never include URLs (which carry codes and state).

use std::fmt;
use std::time::Duration;

use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";

/// Upload videos.
pub const SCOPE_UPLOAD: &str = "https://www.googleapis.com/auth/youtube.upload";
/// Read the channel and its videos: needed for the channel name, processing status, finding a
/// video after a lost response, and view counts.
pub const SCOPE_READONLY: &str = "https://www.googleapis.com/auth/youtube.readonly";
pub const SCOPES: [&str; 2] = [SCOPE_UPLOAD, SCOPE_READONLY];

const CLIENT_ID_SUFFIX: &str = ".apps.googleusercontent.com";
const SERVICE: &str = "Google";

/// The owner's "Desktop app" OAuth client from the Google Cloud console.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &"<redacted>")
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

impl ClientConfig {
    /// Checks the shape of what was pasted (not whether Google accepts it).
    pub fn new(client_id: &str, client_secret: &str) -> Result<Self, String> {
        let client_id = client_id.trim();
        let client_secret = client_secret.trim();
        if !client_id.ends_with(CLIENT_ID_SUFFIX) || client_id.len() <= CLIENT_ID_SUFFIX.len() {
            return Err(format!(
                "the client ID should end with {CLIENT_ID_SUFFIX}; copy it from the Desktop app \
                 client in the Google Cloud console"
            ));
        }
        if client_secret.is_empty() || client_secret.chars().any(char::is_whitespace) {
            return Err("paste the client secret shown next to the client ID".to_owned());
        }
        Ok(Self {
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleEndpoints {
    pub auth: Url,
    pub token: Url,
    pub revoke: Url,
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        Self {
            auth: Url::parse(AUTH_URL).expect("valid URL"),
            token: Url::parse(TOKEN_URL).expect("valid URL"),
            revoke: Url::parse(REVOKE_URL).expect("valid URL"),
        }
    }
}

/// Google's token response.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct GoogleTokens {
    pub access_token: String,
    /// Seconds.
    pub expires_in: i64,
    /// Only on code exchange (and not always on refresh).
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Space-separated granted scopes.
    #[serde(default)]
    pub scope: String,
    /// Seconds; Google includes it for time-limited grants, such as the 7 days of a consent
    /// screen in Testing.
    #[serde(default)]
    pub refresh_token_expires_in: Option<i64>,
}

impl fmt::Debug for GoogleTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleTokens")
            .field("access_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl GoogleTokens {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scope.split_whitespace().any(|s| s == scope)
    }

    /// Requested scopes the user didn't grant (the consent screen lets them untick some).
    pub fn missing_scopes(&self) -> Vec<&'static str> {
        SCOPES.into_iter().filter(|s| !self.has_scope(s)).collect()
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum OAuthError {
    /// The refresh token expired or was revoked (`invalid_grant`): sign in again.
    #[error("Google needs you to sign in again: {message}")]
    Reauthorize { message: String },
    /// Google doesn't recognise the client ID or secret.
    #[error("Google rejected the OAuth client ID or secret: {message}")]
    BadClient { message: String },
    /// The user cancelled or refused in the browser.
    #[error("sign-in was not completed: {message}")]
    Denied { message: String },
    #[error("temporary problem reaching Google: {message}")]
    Transient { message: String },
    #[error("Google rejected the request ({code}): {message}")]
    Rejected { code: String, message: String },
}

impl OAuthError {
    fn from_reqwest(e: &reqwest::Error) -> Self {
        let message = if e.is_timeout() {
            "timed out"
        } else if e.is_connect() {
            "could not connect"
        } else if e.is_decode() {
            "unexpected response"
        } else {
            "request failed"
        };
        Self::Transient {
            message: format!("{SERVICE}: {message}"),
        }
    }
}

#[derive(Deserialize)]
struct TokenErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// Talks to Google's OAuth endpoints.
#[derive(Debug, Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    endpoints: GoogleEndpoints,
}

impl OAuthClient {
    pub fn new(http: reqwest::Client, endpoints: GoogleEndpoints) -> Self {
        Self { http, endpoints }
    }

    /// The consent page to open in the browser. `access_type=offline` and `prompt=consent` make
    /// Google return a refresh token every time, including on reconnect.
    pub fn authorization_url(
        &self,
        client: &ClientConfig,
        redirect_uri: &str,
        state: &str,
        challenge: &str,
    ) -> Url {
        let mut url = self.endpoints.auth.clone();
        url.query_pairs_mut()
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &SCOPES.join(" "))
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        url
    }

    pub async fn exchange_code(
        &self,
        client: &ClientConfig,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<GoogleTokens, OAuthError> {
        self.token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("client_id", &client.client_id),
            ("client_secret", &client.client_secret),
        ])
        .await
    }

    pub async fn refresh(
        &self,
        client: &ClientConfig,
        refresh_token: &str,
    ) -> Result<GoogleTokens, OAuthError> {
        self.token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &client.client_id),
            ("client_secret", &client.client_secret),
        ])
        .await
    }

    /// Revokes the grant (a refresh token revokes its access tokens too).
    pub async fn revoke(&self, token: &str) -> Result<(), OAuthError> {
        let response = self
            .http
            .post(self.endpoints.revoke.clone())
            .form(&[("token", token)])
            .send()
            .await
            .map_err(|e| OAuthError::from_reqwest(&e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(token_error(status, response).await)
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<GoogleTokens, OAuthError> {
        let response = self
            .http
            .post(self.endpoints.token.clone())
            .form(form)
            .send()
            .await
            .map_err(|e| OAuthError::from_reqwest(&e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(token_error(status, response).await);
        }
        response
            .json()
            .await
            .map_err(|e| OAuthError::from_reqwest(&e))
    }
}

async fn token_error(status: StatusCode, response: reqwest::Response) -> OAuthError {
    let body: Option<TokenErrorBody> = response.json().await.ok();
    let (code, message) = body
        .map(|b| (b.error, b.error_description))
        .unwrap_or_default();
    let message = if message.is_empty() {
        format!("HTTP {}", status.as_u16())
    } else {
        message
    };
    match code.as_str() {
        "invalid_grant" => OAuthError::Reauthorize { message },
        "invalid_client" | "unauthorized_client" => OAuthError::BadClient { message },
        _ if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
            OAuthError::Transient { message }
        }
        "" => OAuthError::Rejected {
            code: format!("http_{}", status.as_u16()),
            message,
        },
        _ => OAuthError::Rejected { code, message },
    }
}

/// The local end of the loopback redirect: `http://127.0.0.1:<port>`, on a port picked by the
/// OS, accepting connections from this machine only.
pub struct Loopback {
    listener: TcpListener,
    redirect_uri: String,
}

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

const PAGE_DONE: &str =
    "Shorts Circuit is connected. You can close this tab and go back to the app.";
const PAGE_DENIED: &str =
    "Sign-in was cancelled. You can close this tab and try again from the app.";
const PAGE_WRONG_STATE: &str =
    "This sign-in link doesn't belong to the app's current sign-in. Start again from the app.";

impl Loopback {
    pub async fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}"),
        })
    }

    /// Listens on a fixed port, for providers that only accept a pre-registered redirect
    /// address (`http://127.0.0.1:<port><path>`). Requests are matched by `state`, not path.
    /// Port `0` picks a free port.
    ///
    /// A sign-in that was just cancelled may still be releasing the port, so a busy port is
    /// retried for a few seconds before giving up.
    pub async fn bind_port(port: u16, path: &str) -> std::io::Result<Self> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let listener = loop {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => break listener,
                Err(e)
                    if e.kind() == std::io::ErrorKind::AddrInUse
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(e),
            }
        };
        let port = listener.local_addr()?.port();
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}{path}"),
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Waits for the provider to redirect the browser back with an authorization code for
    /// `expected_state`. Requests with another state (or none) are answered and ignored, so a
    /// stray page can't end or hijack the sign-in. The caller bounds the wait.
    pub async fn wait_for_code(&self, expected_state: &str) -> Result<String, OAuthError> {
        loop {
            let Ok((mut stream, _)) = self.listener.accept().await else {
                continue;
            };
            let Ok(Ok(target)) =
                tokio::time::timeout(READ_TIMEOUT, read_request_target(&mut stream)).await
            else {
                continue;
            };
            let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
                respond(&mut stream, "400 Bad Request", PAGE_WRONG_STATE).await;
                continue;
            };
            let param = |name: &str| {
                url.query_pairs()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.into_owned())
            };
            let Some(state) = param("state") else {
                // E.g. the browser asking for /favicon.ico.
                respond(&mut stream, "404 Not Found", "Not found.").await;
                continue;
            };
            if !constant_time_eq(state.as_bytes(), expected_state.as_bytes()) {
                respond(&mut stream, "400 Bad Request", PAGE_WRONG_STATE).await;
                continue;
            }
            if let Some(error) = param("error") {
                respond(&mut stream, "200 OK", PAGE_DENIED).await;
                return Err(OAuthError::Denied {
                    message: if error == "access_denied" {
                        "access was not granted".to_owned()
                    } else {
                        error
                    },
                });
            }
            match param("code") {
                Some(code) if !code.is_empty() => {
                    respond(&mut stream, "200 OK", PAGE_DONE).await;
                    return Ok(code);
                }
                _ => respond(&mut stream, "400 Bad Request", PAGE_WRONG_STATE).await,
            }
        }
    }
}

/// Reads the request line and headers; returns the request target (`/path?query`).
async fn read_request_target(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).await?;
        if n == 0 || buf.len() + n > MAX_REQUEST_BYTES {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) if target.starts_with('/') => Ok(target.to_owned()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a GET request",
        )),
    }
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Shorts Circuit</title></head>\
         <body style=\"font-family:system-ui,sans-serif;margin:3rem\"><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> ClientConfig {
        ClientConfig::new("123-abc.apps.googleusercontent.com", "GOCSPX-test").unwrap()
    }

    #[test]
    fn client_config_checks_the_pasted_values() {
        assert!(ClientConfig::new(" 1-x.apps.googleusercontent.com ", " s ").is_ok());
        assert!(ClientConfig::new("not-a-client-id", "s").is_err());
        assert!(ClientConfig::new(".apps.googleusercontent.com", "s").is_err());
        assert!(ClientConfig::new("1-x.apps.googleusercontent.com", "").is_err());
        let shown = format!("{:?}", client());
        assert!(!shown.contains("GOCSPX") && !shown.contains("123-abc"));
    }

    #[test]
    fn authorization_url_asks_for_offline_access_with_pkce() {
        let oauth = OAuthClient::new(reqwest::Client::new(), GoogleEndpoints::default());
        let url = oauth.authorization_url(&client(), "http://127.0.0.1:5000", "st", "ch");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5000");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "ch");
        assert_eq!(q["state"], "st");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "consent");
        assert_eq!(q["scope"], SCOPES.join(" "));
        assert!(
            !url.as_str().contains("GOCSPX"),
            "the secret never goes to the browser"
        );
    }

    #[test]
    fn missing_scopes_are_reported() {
        let tokens: GoogleTokens = serde_json::from_str(&format!(
            r#"{{"access_token":"a","expires_in":3599,"scope":"{SCOPE_UPLOAD}"}}"#
        ))
        .unwrap();
        assert_eq!(tokens.missing_scopes(), vec![SCOPE_READONLY]);
        assert!(!format!("{tokens:?}").contains("\"a\""));
    }

    async fn get(uri: &str, target: &str) -> String {
        let addr = uri.trim_start_matches("http://");
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn loopback_ignores_wrong_state_and_returns_the_code() {
        let lb = Loopback::bind().await.unwrap();
        let uri = lb.redirect_uri().to_owned();
        assert!(uri.starts_with("http://127.0.0.1:"));
        let browser = tokio::spawn(async move {
            let favicon = get(&uri, "/favicon.ico").await;
            let forged = get(&uri, "/?state=other&code=evil").await;
            let real = get(&uri, "/?state=right&code=4%2Fgood&scope=x").await;
            (favicon, forged, real)
        });
        let code = lb.wait_for_code("right").await.unwrap();
        assert_eq!(code, "4/good");
        let (favicon, forged, real) = browser.await.unwrap();
        assert!(favicon.starts_with("HTTP/1.1 404"));
        assert!(forged.starts_with("HTTP/1.1 400"));
        assert!(real.starts_with("HTTP/1.1 200") && real.contains("connected"));
    }

    #[tokio::test]
    async fn loopback_reports_a_denied_consent() {
        let lb = Loopback::bind().await.unwrap();
        let uri = lb.redirect_uri().to_owned();
        tokio::spawn(async move { get(&uri, "/?state=s&error=access_denied").await });
        assert!(matches!(
            lb.wait_for_code("s").await,
            Err(OAuthError::Denied { .. })
        ));
    }
}
