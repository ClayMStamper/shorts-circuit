//! Client for the OAuth broker (`shorts-backend`) and the wire types both sides share.
//!
//! The broker holds the confidential client secret, so authorization-code exchange and token
//! refresh happen there. The desktop app opens [`BrokerClient::connect_url`] in the browser,
//! then collects the tokens with [`BrokerClient::claim`], proving it started the flow with a
//! PKCE-style verifier whose SHA-256 was in the connect URL. Tokens are never logged; the
//! `Debug` impl of [`TokenSet`] redacts them.

use std::fmt;

use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::tiktok::IntakeError;

const SERVICE: &str = "the OAuth broker";

/// Tokens as TikTok's token endpoint returns them, relayed unchanged by the broker.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    /// Seconds.
    pub expires_in: i64,
    pub refresh_token: String,
    /// Seconds.
    #[serde(default)]
    pub refresh_expires_in: i64,
    pub open_id: String,
    #[serde(default)]
    pub scope: String,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .field("refresh_token", &"<redacted>")
            .field("refresh_expires_in", &self.refresh_expires_in)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// `GET /v1/tiktok/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    /// A TikTok developer app (client key and secret) is configured on the broker.
    pub configured: bool,
}

/// `POST /v1/tiktok/claim`.
#[derive(Clone, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub state: String,
    pub verifier: String,
}

/// `POST /v1/tiktok/refresh`.
#[derive(Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// `POST /v1/tiktok/revoke`.
#[derive(Clone, Serialize, Deserialize)]
pub struct RevokeRequest {
    pub access_token: String,
}

/// Error body for non-2xx broker responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerError {
    pub error: String,
    #[serde(default)]
    pub message: String,
}

/// The broker's answer to a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// The user hasn't finished authorizing in the browser yet.
    Pending,
    Ready(TokenSet),
    /// Denied, expired, or not started by this app.
    Failed {
        message: String,
    },
}

/// `base64url(sha256(verifier))` without padding, as in PKCE S256.
pub fn challenge_for(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url(&digest)
}

/// Unpadded base64url, for verifiers, challenges and state values.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct BrokerClient {
    http: reqwest::Client,
    base: Url,
}

impl BrokerClient {
    pub fn new(http: reqwest::Client, base: Url) -> Self {
        Self { http, base }
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}{path}"));
        url.set_query(None);
        url
    }

    pub async fn status(&self) -> Result<ProviderStatus, IntakeError> {
        let response = self
            .http
            .get(self.url("/v1/tiktok/status"))
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        json(response).await
    }

    /// The page to open in the browser to start authorization.
    pub fn connect_url(&self, state: &str, challenge: &str) -> Url {
        let mut url = self.url("/v1/tiktok/connect");
        url.query_pairs_mut()
            .append_pair("state", state)
            .append_pair("challenge", challenge);
        url
    }

    pub async fn claim(&self, state: &str, verifier: &str) -> Result<Claim, IntakeError> {
        let response = self
            .http
            .post(self.url("/v1/tiktok/claim"))
            .json(&ClaimRequest {
                state: state.to_owned(),
                verifier: verifier.to_owned(),
            })
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        match response.status() {
            StatusCode::ACCEPTED => Ok(Claim::Pending),
            s if s.is_success() => Ok(Claim::Ready(json(response).await?)),
            s if s.is_client_error() => {
                let body: Option<BrokerError> = response.json().await.ok();
                Ok(Claim::Failed {
                    message: body
                        .map(|b| b.message)
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| "authorization was not completed".into()),
                })
            }
            s => Err(IntakeError::Transient {
                service: SERVICE,
                message: format!("HTTP {}", s.as_u16()),
            }),
        }
    }

    /// Exchanges a refresh token for new tokens.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenSet, IntakeError> {
        let response = self
            .http
            .post(self.url("/v1/tiktok/refresh"))
            .json(&RefreshRequest {
                refresh_token: refresh_token.to_owned(),
            })
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        json(response).await
    }

    /// Asks TikTok (through the broker) to revoke the access token.
    pub async fn revoke(&self, access_token: &str) -> Result<(), IntakeError> {
        let response = self
            .http
            .post(self.url("/v1/tiktok/revoke"))
            .json(&RevokeRequest {
                access_token: access_token.to_owned(),
            })
            .send()
            .await
            .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(error_from(response).await)
        }
    }
}

async fn json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, IntakeError> {
    if !response.status().is_success() {
        return Err(error_from(response).await);
    }
    response
        .json()
        .await
        .map_err(|e| IntakeError::from_reqwest(SERVICE, &e))
}

async fn error_from(response: reqwest::Response) -> IntakeError {
    let status = response.status();
    let body: Option<BrokerError> = response.json().await.ok();
    let (code, message) = body
        .map(|b| (b.error, b.message))
        .unwrap_or_else(|| (format!("http_{}", status.as_u16()), String::new()));
    if status == StatusCode::UNAUTHORIZED || code == "reauthorize" {
        IntakeError::Unauthorized { message }
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        IntakeError::RateLimited { retry_after: None }
    } else if status.is_server_error() || status == StatusCode::BAD_GATEWAY {
        IntakeError::Transient {
            service: SERVICE,
            message: if message.is_empty() { code } else { message },
        }
    } else {
        IntakeError::Rejected {
            service: SERVICE,
            code,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_the_rfc_7636_example() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn token_debug_is_redacted() {
        let t = TokenSet {
            access_token: "act.secret".into(),
            expires_in: 86400,
            refresh_token: "rft.secret".into(),
            refresh_expires_in: 1,
            open_id: "someone".into(),
            scope: "user.info.basic,video.list".into(),
        };
        let shown = format!("{t:?}");
        assert!(!shown.contains("secret"));
        assert!(!shown.contains("someone"));
    }

    #[test]
    fn paths_join_under_the_base() {
        let c = BrokerClient::new(
            reqwest::Client::new(),
            Url::parse("https://broker.example/prefix/").unwrap(),
        );
        assert_eq!(
            c.connect_url("s", "c").as_str(),
            "https://broker.example/prefix/v1/tiktok/connect?state=s&challenge=c"
        );
    }
}
