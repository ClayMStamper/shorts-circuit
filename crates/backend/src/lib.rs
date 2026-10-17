//! OAuth broker. It keeps confidential client credentials off the desktop: the desktop app
//! starts an authorization flow here, TikTok redirects back here, the broker exchanges the
//! code using the client secret, and the desktop collects the tokens with a verifier only it
//! knows. Refresh and revoke also go through the broker for the same reason.
//!
//! Only TikTok (as a source, for new-post detection) is wired up so far. Pending flows live in
//! memory for ten minutes; nothing is persisted. Tokens, codes and state values are never
//! logged.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use reqwest::Url;
use serde::Deserialize;
use shorts_adapters::broker::{
    BrokerError, ClaimRequest, ProviderStatus, RefreshRequest, RevokeRequest, TokenSet,
    challenge_for,
};
use tracing::{info, warn};

/// Scopes the desktop needs: profile name and the public post list. Nothing that can post.
pub const TIKTOK_SCOPES: &str = "user.info.basic,video.list";
const FLOW_TTL: Duration = Duration::from_secs(600);
const MAX_FLOWS: usize = 1_000;

/// A registered TikTok developer app.
#[derive(Clone)]
pub struct TikTokApp {
    pub client_key: String,
    pub client_secret: String,
    /// Must be registered in the TikTok developer portal: `<broker>/v1/tiktok/callback`.
    pub redirect_uri: Url,
    pub authorize_url: Url,
    pub token_url: Url,
    pub revoke_url: Url,
}

impl fmt::Debug for TikTokApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TikTokApp")
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri.as_str())
            .finish_non_exhaustive()
    }
}

impl TikTokApp {
    pub const AUTHORIZE_URL: &'static str = "https://www.tiktok.com/v2/auth/authorize/";
    pub const TOKEN_URL: &'static str = "https://open.tiktokapis.com/v2/oauth/token/";
    pub const REVOKE_URL: &'static str = "https://open.tiktokapis.com/v2/oauth/revoke/";
}

#[derive(Debug, Clone, Default)]
pub struct BrokerConfig {
    pub tiktok: Option<TikTokApp>,
}

impl BrokerConfig {
    /// Reads `TIKTOK_CLIENT_KEY`, `TIKTOK_CLIENT_SECRET` and `TIKTOK_REDIRECT_URI` (and the
    /// optional endpoint overrides). Without the key and secret TikTok is reported as not
    /// configured.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let (Some(key), Some(secret)) = (var("TIKTOK_CLIENT_KEY"), var("TIKTOK_CLIENT_SECRET"))
        else {
            return Ok(Self { tiktok: None });
        };
        let url = |name: &str, default: Option<&str>| -> Result<Url, String> {
            let raw = var(name)
                .or_else(|| default.map(str::to_owned))
                .ok_or_else(|| format!("{name} is required when TIKTOK_CLIENT_KEY is set"))?;
            Url::parse(&raw).map_err(|e| format!("{name}: {e}"))
        };
        Ok(Self {
            tiktok: Some(TikTokApp {
                client_key: key,
                client_secret: secret,
                redirect_uri: url("TIKTOK_REDIRECT_URI", None)?,
                authorize_url: url("TIKTOK_AUTHORIZE_URL", Some(TikTokApp::AUTHORIZE_URL))?,
                token_url: url("TIKTOK_TOKEN_URL", Some(TikTokApp::TOKEN_URL))?,
                revoke_url: url("TIKTOK_REVOKE_URL", Some(TikTokApp::REVOKE_URL))?,
            }),
        })
    }
}

enum Outcome {
    Waiting,
    Ready(TokenSet),
    Failed(String),
}

struct Flow {
    challenge: String,
    created: Instant,
    outcome: Outcome,
}

struct Inner {
    config: BrokerConfig,
    http: reqwest::Client,
    flows: Mutex<HashMap<String, Flow>>,
}

type Shared = Arc<Inner>;

/// The broker's routes.
pub fn router(config: BrokerConfig, http: reqwest::Client) -> Router {
    let state = Arc::new(Inner {
        config,
        http,
        flows: Mutex::new(HashMap::new()),
    });
    Router::new()
        .route("/v1/tiktok/status", get(status))
        .route("/v1/tiktok/connect", get(connect))
        .route("/v1/tiktok/callback", get(callback))
        .route("/v1/tiktok/claim", post(claim))
        .route("/v1/tiktok/refresh", post(refresh))
        .route("/v1/tiktok/revoke", post(revoke))
        .with_state(state)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(BrokerError {
            error: code.to_owned(),
            message: message.to_owned(),
        }),
    )
        .into_response()
}

fn not_configured() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "not_configured",
        "No TikTok developer app is configured on this broker.",
    )
}

/// URL-safe random-looking values of sensible length; anything else is refused.
fn valid_token(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn status(State(s): State<Shared>) -> Json<ProviderStatus> {
    Json(ProviderStatus {
        configured: s.config.tiktok.is_some(),
    })
}

#[derive(Deserialize)]
struct ConnectQuery {
    state: String,
    challenge: String,
}

async fn connect(State(s): State<Shared>, Query(q): Query<ConnectQuery>) -> Response {
    let Some(app) = &s.config.tiktok else {
        return not_configured();
    };
    if !valid_token(&q.state, 22, 128) || !valid_token(&q.challenge, 43, 43) {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Invalid state or challenge.",
        );
    }
    {
        let mut flows = s.flows.lock().expect("flows lock");
        flows.retain(|_, f| f.created.elapsed() < FLOW_TTL);
        if flows.len() >= MAX_FLOWS {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                "Too many pending sign-ins.",
            );
        }
        if flows.contains_key(&q.state) {
            return error(
                StatusCode::CONFLICT,
                "state_in_use",
                "Start the connection again.",
            );
        }
        flows.insert(
            q.state.clone(),
            Flow {
                challenge: q.challenge,
                created: Instant::now(),
                outcome: Outcome::Waiting,
            },
        );
    }
    let mut url = app.authorize_url.clone();
    url.query_pairs_mut()
        .append_pair("client_key", &app.client_key)
        .append_pair("scope", TIKTOK_SCOPES)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", app.redirect_uri.as_str())
        .append_pair("state", &q.state);
    Redirect::to(url.as_str()).into_response()
}

#[derive(Deserialize)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\">\
         <h1>{title}</h1><p>{body}</p></body></html>"
    );
    (status, [(header::CACHE_CONTROL, "no-store")], Html(html)).into_response()
}

async fn callback(State(s): State<Shared>, Query(q): Query<CallbackQuery>) -> Response {
    let Some(app) = &s.config.tiktok else {
        return not_configured();
    };
    let Some(state) = q.state.filter(|st| valid_token(st, 22, 128)) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in failed",
            "The link is incomplete.",
        );
    };
    let waiting = s
        .flows
        .lock()
        .expect("flows lock")
        .get(&state)
        .is_some_and(|f| matches!(f.outcome, Outcome::Waiting) && f.created.elapsed() < FLOW_TTL);
    if !waiting {
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in expired",
            "Start connecting TikTok again from Shorts Circuit.",
        );
    }
    let outcome = match (q.code, q.error) {
        (Some(code), None) => match exchange(&s.http, app, &code).await {
            Ok(tokens) => Outcome::Ready(tokens),
            Err(message) => Outcome::Failed(message),
        },
        (_, error) => Outcome::Failed(
            q.error_description
                .or(error)
                .unwrap_or_else(|| "TikTok did not return an authorization code.".into()),
        ),
    };
    let ok = matches!(outcome, Outcome::Ready(_));
    if let Some(flow) = s.flows.lock().expect("flows lock").get_mut(&state) {
        flow.outcome = outcome;
    }
    if ok {
        info!("tiktok authorization completed");
        page(
            StatusCode::OK,
            "TikTok connected",
            "You can close this tab and return to Shorts Circuit.",
        )
    } else {
        warn!("tiktok authorization failed");
        page(
            StatusCode::OK,
            "TikTok was not connected",
            "Return to Shorts Circuit for details and try again.",
        )
    }
}

/// What TikTok's token endpoint returns: tokens, or an error description.
#[derive(Deserialize)]
struct TokenResponse {
    #[serde(flatten)]
    tokens: Option<TokenSet>,
    error: Option<String>,
    error_description: Option<String>,
}

enum ExchangeError {
    /// TikTok refused the grant (bad code, revoked refresh token).
    Refused(String),
    Unreachable,
}

async fn token_request(
    http: &reqwest::Client,
    app: &TikTokApp,
    grant: &[(&str, &str)],
) -> Result<TokenSet, ExchangeError> {
    let mut form = vec![
        ("client_key", app.client_key.as_str()),
        ("client_secret", app.client_secret.as_str()),
    ];
    form.extend_from_slice(grant);
    let response = http
        .post(app.token_url.clone())
        .form(&form)
        .send()
        .await
        .map_err(|_| ExchangeError::Unreachable)?;
    let status = response.status();
    let body: TokenResponse = response
        .json()
        .await
        .map_err(|_| ExchangeError::Unreachable)?;
    match body.tokens {
        Some(tokens) if status.is_success() && body.error.as_deref().unwrap_or("").is_empty() => {
            Ok(tokens)
        }
        _ if status.is_server_error() => Err(ExchangeError::Unreachable),
        _ => Err(ExchangeError::Refused(
            body.error_description
                .or(body.error)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16())),
        )),
    }
}

async fn exchange(http: &reqwest::Client, app: &TikTokApp, code: &str) -> Result<TokenSet, String> {
    let redirect = app.redirect_uri.as_str();
    token_request(
        http,
        app,
        &[
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect),
        ],
    )
    .await
    .map_err(|e| match e {
        ExchangeError::Refused(m) => m,
        ExchangeError::Unreachable => "TikTok could not be reached; try again.".into(),
    })
}

async fn claim(State(s): State<Shared>, Json(req): Json<ClaimRequest>) -> Response {
    let mut flows = s.flows.lock().expect("flows lock");
    let Some(flow) = flows.get(&req.state) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_state",
            "Start connecting again.",
        );
    };
    if flow.created.elapsed() >= FLOW_TTL {
        flows.remove(&req.state);
        return error(
            StatusCode::GONE,
            "expired",
            "The sign-in took too long; start again.",
        );
    }
    if challenge_for(&req.verifier) != flow.challenge {
        // A wrong verifier ends the flow so it can't be guessed at.
        flows.remove(&req.state);
        return error(
            StatusCode::FORBIDDEN,
            "verifier_mismatch",
            "Start connecting again.",
        );
    }
    if matches!(flow.outcome, Outcome::Waiting) {
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "status": "pending" })),
        )
            .into_response();
    }
    match flows.remove(&req.state).map(|f| f.outcome) {
        Some(Outcome::Ready(tokens)) => Json(tokens).into_response(),
        Some(Outcome::Failed(message)) => error(StatusCode::BAD_REQUEST, "denied", &message),
        _ => error(
            StatusCode::NOT_FOUND,
            "unknown_state",
            "Start connecting again.",
        ),
    }
}

async fn refresh(State(s): State<Shared>, Json(req): Json<RefreshRequest>) -> Response {
    let Some(app) = &s.config.tiktok else {
        return not_configured();
    };
    match token_request(
        &s.http,
        app,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", req.refresh_token.as_str()),
        ],
    )
    .await
    {
        Ok(tokens) => Json(tokens).into_response(),
        Err(ExchangeError::Refused(message)) => {
            error(StatusCode::UNAUTHORIZED, "reauthorize", &message)
        }
        Err(ExchangeError::Unreachable) => error(
            StatusCode::BAD_GATEWAY,
            "upstream",
            "TikTok could not be reached.",
        ),
    }
}

async fn revoke(State(s): State<Shared>, Json(req): Json<RevokeRequest>) -> Response {
    let Some(app) = &s.config.tiktok else {
        return not_configured();
    };
    let form = [
        ("client_key", app.client_key.as_str()),
        ("client_secret", app.client_secret.as_str()),
        ("token", req.access_token.as_str()),
    ];
    match s.http.post(app.revoke_url.clone()).form(&form).send().await {
        Ok(r) if r.status().is_success() => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => error(
            StatusCode::BAD_GATEWAY,
            "upstream",
            "TikTok refused to revoke.",
        ),
        Err(_) => error(
            StatusCode::BAD_GATEWAY,
            "upstream",
            "TikTok could not be reached.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_debug_hides_the_secret() {
        let app = TikTokApp {
            client_key: "key".into(),
            client_secret: "very-secret".into(),
            redirect_uri: Url::parse("https://broker.example/v1/tiktok/callback").unwrap(),
            authorize_url: Url::parse(TikTokApp::AUTHORIZE_URL).unwrap(),
            token_url: Url::parse(TikTokApp::TOKEN_URL).unwrap(),
            revoke_url: Url::parse(TikTokApp::REVOKE_URL).unwrap(),
        };
        assert!(!format!("{app:?}").contains("very-secret"));
    }

    #[test]
    fn state_and_challenge_shapes_are_checked() {
        assert!(valid_token(&"a".repeat(32), 22, 128));
        assert!(!valid_token("short", 22, 128));
        assert!(!valid_token(&format!("{}&x=1", "a".repeat(30)), 22, 128));
    }
}
