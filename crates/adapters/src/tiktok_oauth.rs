//! TikTok Login Kit for desktop, used with the owner's own TikTok developer app: the browser
//! sign-in returns to a loopback address on this computer and the desktop exchanges the code
//! and refreshes tokens itself, so no hosted OAuth broker is needed.
//!
//! What TikTok's docs say (read 21 October 2026, "Login Kit for Desktop" and "Manage user
//! access tokens"): desktop apps register a redirect URI on `localhost` or `127.0.0.1` with an
//! explicit port (a `*` port is allowed) and use PKCE with `code_challenge_method=S256`; the
//! token endpoint is `POST https://open.tiktokapis.com/v2/oauth/token/` with `client_key`,
//! `client_secret`, `code`, `grant_type`, `redirect_uri` and (desktop and mobile) the
//! `code_verifier`. The client secret is still required, so the owner pastes their app's key
//! and secret into the app; both stay in the OS credential store and only go to TikTok's token
//! endpoint, like the Snap OAuth app.
//!
//! Assumption to confirm with a real sign-in: the desktop page describes the challenge as the
//! *hex* SHA-256 of the verifier, unlike RFC 7636's base64url. [`ChallengeEncoding::Hex`] is the
//! default; `SHORTS_TIKTOK_PKCE=base64url` switches if TikTok rejects the verifier.

use std::fmt;

use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::broker::{TokenSet, base64url};
use crate::tiktok::IntakeError;

pub const AUTHORIZE_URL: &str = "https://www.tiktok.com/v2/auth/authorize/";
pub const TOKEN_URL: &str = "https://open.tiktokapis.com/v2/oauth/token/";
pub const REVOKE_URL: &str = "https://open.tiktokapis.com/v2/oauth/revoke/";
/// Read-only: the profile name and the public post list. Nothing that can post.
pub const SCOPES: &str = "user.info.basic,video.list";

const SERVICE: &str = "TikTok sign-in";

/// The owner's TikTok developer app. `Debug` hides the secret.
#[derive(Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct TikTokApp {
    pub client_key: String,
    pub client_secret: String,
}

impl fmt::Debug for TikTokApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TikTokApp")
            .field("client_key", &self.client_key)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

impl TikTokApp {
    /// Checks pasted values: both present, no spaces inside.
    pub fn new(client_key: &str, client_secret: &str) -> Result<Self, String> {
        let key = client_key.trim();
        let secret = client_secret.trim();
        if key.is_empty() || secret.is_empty() {
            return Err("paste both the client key and the client secret".into());
        }
        if key.contains(char::is_whitespace) || secret.contains(char::is_whitespace) {
            return Err("the client key and secret don't contain spaces; copy them again".into());
        }
        Ok(Self {
            client_key: key.to_owned(),
            client_secret: secret.to_owned(),
        })
    }
}

/// How the PKCE challenge is written (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChallengeEncoding {
    #[default]
    Hex,
    Base64Url,
}

impl ChallengeEncoding {
    /// From `SHORTS_TIKTOK_PKCE` (`hex` or `base64url`).
    pub fn from_env() -> Self {
        match std::env::var("SHORTS_TIKTOK_PKCE").as_deref() {
            Ok("base64url") => Self::Base64Url,
            _ => Self::Hex,
        }
    }

    pub fn challenge(self, verifier: &str) -> String {
        let digest = Sha256::digest(verifier.as_bytes());
        match self {
            Self::Hex => digest.iter().map(|b| format!("{b:02x}")).collect(),
            Self::Base64Url => base64url(&digest),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TikTokEndpoints {
    pub authorize: Url,
    pub token: Url,
    pub revoke: Url,
}

impl Default for TikTokEndpoints {
    fn default() -> Self {
        Self {
            authorize: Url::parse(AUTHORIZE_URL).expect("valid URL"),
            token: Url::parse(TOKEN_URL).expect("valid URL"),
            revoke: Url::parse(REVOKE_URL).expect("valid URL"),
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(flatten)]
    tokens: Option<TokenSet>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TikTokOAuth {
    http: reqwest::Client,
    endpoints: TikTokEndpoints,
    encoding: ChallengeEncoding,
}

impl TikTokOAuth {
    pub fn new(
        http: reqwest::Client,
        endpoints: TikTokEndpoints,
        encoding: ChallengeEncoding,
    ) -> Self {
        Self {
            http,
            endpoints,
            encoding,
        }
    }

    /// The consent page to open in the browser.
    pub fn authorization_url(
        &self,
        app: &TikTokApp,
        redirect_uri: &str,
        state: &str,
        verifier: &str,
    ) -> Url {
        let mut url = self.endpoints.authorize.clone();
        url.query_pairs_mut()
            .append_pair("client_key", &app.client_key)
            .append_pair("scope", SCOPES)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", &self.encoding.challenge(verifier))
            .append_pair("code_challenge_method", "S256");
        url
    }

    pub async fn exchange_code(
        &self,
        app: &TikTokApp,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<TokenSet, IntakeError> {
        self.token_request(
            app,
            &[
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ],
        )
        .await
    }

    pub async fn refresh(
        &self,
        app: &TikTokApp,
        refresh_token: &str,
    ) -> Result<TokenSet, IntakeError> {
        self.token_request(
            app,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
        .await
    }

    /// Best effort: TikTok forgets the grant.
    pub async fn revoke(&self, app: &TikTokApp, access_token: &str) -> Result<(), IntakeError> {
        let form = [
            ("client_key", app.client_key.as_str()),
            ("client_secret", app.client_secret.as_str()),
            ("token", access_token),
        ];
        let response = self
            .http
            .post(self.endpoints.revoke.clone())
            .form(&form)
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(IntakeError::Rejected {
                service: SERVICE,
                code: response.status().as_u16().to_string(),
                message: "TikTok did not revoke the sign-in".into(),
            })
        }
    }

    async fn token_request(
        &self,
        app: &TikTokApp,
        grant: &[(&str, &str)],
    ) -> Result<TokenSet, IntakeError> {
        let mut form = vec![
            ("client_key", app.client_key.as_str()),
            ("client_secret", app.client_secret.as_str()),
        ];
        form.extend_from_slice(grant);
        let response = self
            .http
            .post(self.endpoints.token.clone())
            .form(&form)
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        let status = response.status();
        if status.is_server_error() {
            return Err(IntakeError::Transient {
                service: SERVICE,
                message: format!("HTTP {}", status.as_u16()),
            });
        }
        let body: TokenResponse = response
            .json()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        let error = body.error.filter(|e| !e.is_empty());
        match (body.tokens, error) {
            (Some(tokens), None) if status.is_success() => Ok(tokens),
            (_, error) => {
                let code = error.unwrap_or_else(|| status.as_u16().to_string());
                let message = body
                    .error_description
                    .unwrap_or_else(|| "TikTok refused the request".into());
                // A refused grant (bad or expired code or refresh token) means signing in again;
                // a refused client means the pasted key or secret is wrong.
                if code == "invalid_grant" {
                    Err(IntakeError::Unauthorized { message })
                } else {
                    Err(IntakeError::Rejected {
                        service: SERVICE,
                        code,
                        message,
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_apps_are_checked_and_secrets_hidden() {
        assert!(TikTokApp::new(" ", "x").is_err());
        assert!(TikTokApp::new("a b", "x").is_err());
        let app = TikTokApp::new(" key ", "secret").unwrap();
        assert_eq!(app.client_key, "key");
        assert!(!format!("{app:?}").contains("secret\""));
        assert!(format!("{app:?}").contains("<redacted>"));
    }

    #[test]
    fn challenges_are_hex_by_default() {
        // SHA-256("abc").
        assert_eq!(
            ChallengeEncoding::Hex.challenge("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            ChallengeEncoding::Base64Url.challenge("abc"),
            "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
        );
    }

    #[test]
    fn the_authorization_url_asks_for_read_only_scopes_with_pkce() {
        let oauth = TikTokOAuth::new(
            reqwest::Client::new(),
            TikTokEndpoints::default(),
            ChallengeEncoding::Hex,
        );
        let app = TikTokApp::new("key", "secret").unwrap();
        let url = oauth.authorization_url(&app, "http://127.0.0.1:47616/callback/", "st", "abc");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_key"], "key");
        assert_eq!(q["scope"], "user.info.basic,video.list");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:47616/callback/");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st");
        assert!(!url.as_str().contains("secret"));
    }
}
