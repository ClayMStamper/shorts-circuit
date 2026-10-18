//! The sync API (see `shorts_cloud::protocol`). Every route but `/healthz` needs an
//! installation's bearer token; an installation only sees its own accounts, uploads and
//! publications. Request bodies and credentials are never logged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use shorts_cloud::{
    AccountPut, AccountStatus, ApiError, CredentialCopy, ErrorCode, HandoverPut, MAX_CHUNK_BYTES,
    PublicationStatus, ReleaseRequest, UploadStart, UploadStatus,
};
use shorts_domain::{AccountId, PublicationId};
use tracing::{error, info, warn};

use crate::db::{DbError, Installation, Received};
use crate::media::valid_sha256;
use crate::runner::Runner;

/// Largest file the runner accepts (`SHORTS_RUNNER_MAX_UPLOAD_BYTES`, default 4 GiB).
fn max_upload_bytes() -> u64 {
    std::env::var("SHORTS_RUNNER_MAX_UPLOAD_BYTES")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(4 * 1024 * 1024 * 1024)
}

#[derive(Clone)]
struct AppState {
    runner: Arc<Runner>,
    /// One chunk at a time per upload.
    upload_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl AppState {
    fn upload_lock(
        &self,
        installation: &Installation,
        sha256: &str,
    ) -> Arc<tokio::sync::Mutex<()>> {
        self.upload_locks
            .lock()
            .expect("upload locks")
            .entry(format!("{}/{sha256}", installation.id))
            .or_default()
            .clone()
    }
}

/// An error answer.
struct Failure(StatusCode, ApiError);

impl Failure {
    fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self(
            status,
            ApiError {
                code,
                message: message.into(),
                received_bytes: None,
                revision: None,
            },
        )
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::Invalid,
            message,
        )
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "not found")
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<DbError> for Failure {
    fn from(e: DbError) -> Self {
        use StatusCode as S;
        if let Some(why) = e.in_flight() {
            return Self::new(
                S::CONFLICT,
                ErrorCode::InFlight,
                format!("the cloud runner can't give it back yet: {why}"),
            );
        }
        match e {
            DbError::NotFound { .. } => Self::not_found(),
            DbError::Conflict { .. } => Self::new(
                S::CONFLICT,
                ErrorCode::Conflict,
                "changed concurrently; try again",
            ),
            DbError::Refused(message) => Self::new(S::CONFLICT, ErrorCode::Conflict, message),
            DbError::HandoverGone => Self::new(
                S::GONE,
                ErrorCode::Gone,
                "that hand-over was already taken back",
            ),
            DbError::MediaIncomplete => Self::new(
                S::CONFLICT,
                ErrorCode::MediaIncomplete,
                "upload the video completely first",
            ),
            DbError::CredentialConflict { revision } => {
                let mut f = Self::new(
                    S::CONFLICT,
                    ErrorCode::CredentialConflict,
                    "the runner refreshed this credential; fetch it first",
                );
                f.1.revision = Some(revision);
                f
            }
            DbError::OffsetMismatch { received } => {
                let mut f = Self::new(
                    S::CONFLICT,
                    ErrorCode::OffsetMismatch,
                    format!("the upload is at byte {received}"),
                );
                f.1.received_bytes = Some(received);
                f
            }
            DbError::Transition(t) => Self::invalid(t.to_string()),
            DbError::Job(j) => Self::new(S::CONFLICT, ErrorCode::Conflict, j.to_string()),
            other => {
                error!(error = %other, "request failed");
                Self::new(
                    S::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "the runner hit an internal error; see its log",
                )
            }
        }
    }
}

type ApiResult<T> = Result<Json<T>, Failure>;

pub fn router(runner: Arc<Runner>) -> Router {
    let state = AppState {
        runner,
        upload_locks: Arc::default(),
    };
    let chunk_limit = usize::try_from(MAX_CHUNK_BYTES).unwrap_or(usize::MAX) + 64 * 1024;
    let api = Router::new()
        .route("/v1/uploads", post(start_upload))
        .route("/v1/uploads/{sha256}", get(upload_status).put(upload_chunk))
        .route("/v1/accounts", get(accounts))
        .route("/v1/accounts/{id}", put(put_account))
        .route("/v1/accounts/{id}/credential", get(credential))
        .route("/v1/publications", get(publications))
        .route("/v1/publications/{id}", get(publication).put(hand_over))
        .route("/v1/publications/{id}/release", post(release))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(DefaultBodyLimit::max(chunk_limit));
    Router::new()
        .route("/healthz", get(health))
        .merge(api)
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Response {
    match sqlx::query("SELECT 1")
        .execute(state.runner.store.pool())
        .await
    {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response(),
    }
}

async fn authenticate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    let refused = || {
        Failure::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "unknown or revoked installation token",
        )
        .into_response()
    };
    if token.is_empty() {
        return refused();
    }
    match state
        .runner
        .store
        .authenticate(&token, state.runner.clock.now())
        .await
    {
        Ok(Some(installation)) => {
            request.extensions_mut().insert(installation);
            next.run(request).await
        }
        Ok(None) => {
            warn!("refused a request with an unknown or revoked token");
            refused()
        }
        Err(e) => Failure::from(e).into_response(),
    }
}

fn check_sha(sha256: &str) -> Result<(), Failure> {
    if valid_sha256(sha256) {
        Ok(())
    } else {
        Err(Failure::invalid(
            "sha256 must be 64 lowercase hex characters",
        ))
    }
}

async fn start_upload(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Json(body): Json<UploadStart>,
) -> ApiResult<UploadStatus> {
    check_sha(&body.sha256)?;
    if body.size_bytes > max_upload_bytes() {
        return Err(Failure::invalid(format!(
            "the file is larger than this runner accepts ({} bytes)",
            max_upload_bytes()
        )));
    }
    let runner = &state.runner;
    let status = runner
        .store
        .start_upload(
            installation.id,
            &body.sha256,
            body.size_bytes,
            runner.clock.now(),
        )
        .await?;
    if body.size_bytes == 0 && !status.complete {
        let lock = state.upload_lock(&installation, &body.sha256);
        let _guard = lock.lock().await;
        return finish_upload(&state, &installation, &body.sha256).await;
    }
    Ok(Json(status))
}

async fn upload_status(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Path(sha256): Path<String>,
) -> ApiResult<UploadStatus> {
    check_sha(&sha256)?;
    state
        .runner
        .store
        .upload(installation.id, &sha256)
        .await?
        .map(Json)
        .ok_or_else(Failure::not_found)
}

#[derive(Deserialize)]
struct Offset {
    offset: u64,
}

async fn upload_chunk(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Path(sha256): Path<String>,
    Query(Offset { offset }): Query<Offset>,
    body: Bytes,
) -> ApiResult<UploadStatus> {
    check_sha(&sha256)?;
    let runner = &state.runner;
    let lock = state.upload_lock(&installation, &sha256);
    let _guard = lock.lock().await;
    let status = runner
        .store
        .upload(installation.id, &sha256)
        .await?
        .ok_or_else(Failure::not_found)?;
    if status.complete {
        return Ok(Json(status));
    }
    if offset != status.received_bytes {
        return Err(DbError::OffsetMismatch {
            received: status.received_bytes,
        }
        .into());
    }
    let len = body.len() as u64;
    if len == 0 || len > MAX_CHUNK_BYTES {
        return Err(Failure::invalid(format!(
            "send between 1 and {MAX_CHUNK_BYTES} bytes per chunk"
        )));
    }
    let end = offset
        .checked_add(len)
        .filter(|end| *end <= status.size_bytes)
        .ok_or_else(|| Failure::invalid("the chunk goes past the end of the file"))?;
    runner
        .media
        .write_chunk(installation.id, &sha256, offset, &body)
        .await
        .map_err(|e| {
            error!(error = %e, "could not write an upload chunk");
            Failure::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "the runner could not store the chunk",
            )
        })?;
    let status = runner
        .store
        .advance_upload(installation.id, &sha256, offset, end, runner.clock.now())
        .await?;
    if status.received_bytes == status.size_bytes {
        return finish_upload(&state, &installation, &sha256).await;
    }
    Ok(Json(status))
}

async fn finish_upload(
    state: &AppState,
    installation: &Installation,
    sha256: &str,
) -> ApiResult<UploadStatus> {
    let runner = &state.runner;
    let ok = runner
        .media
        .finish(installation.id, sha256)
        .await
        .map_err(|e| {
            error!(error = %e, "could not finish an upload");
            Failure::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "the runner could not store the file",
            )
        })?;
    if !ok {
        runner
            .store
            .reset_upload(installation.id, sha256, runner.clock.now())
            .await?;
        return Err(Failure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ChecksumMismatch,
            "the uploaded bytes don't match the SHA-256; upload the file again",
        ));
    }
    info!(installation = %installation.id, "upload complete");
    Ok(Json(
        runner
            .store
            .complete_upload(installation.id, sha256, runner.clock.now())
            .await?,
    ))
}

async fn accounts(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
) -> ApiResult<Vec<AccountStatus>> {
    Ok(Json(
        state.runner.store.account_statuses(installation.id).await?,
    ))
}

async fn put_account(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Path(id): Path<AccountId>,
    Json(body): Json<AccountPut>,
) -> ApiResult<AccountStatus> {
    if body.account.id != id {
        return Err(Failure::invalid("the account id doesn't match the address"));
    }
    let runner = &state.runner;
    let now = runner.clock.now();
    let (status, changed) = runner
        .store
        .put_account(installation.id, &body, now)
        .await?;
    if changed {
        info!(account = %id, revision = status.credential_revision, "credential updated by the desktop");
        runner.tokens.forget(id);
        let resumed = runner.store.resume_after_reconnect(id, now).await?;
        if !resumed.is_empty() {
            info!(account = %id, count = resumed.len(), "resumed after reconnect");
            runner.poke();
        }
    }
    Ok(Json(status))
}

async fn credential(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Path(id): Path<AccountId>,
) -> ApiResult<CredentialCopy> {
    let store = &state.runner.store;
    if store.account_installation(id).await? != Some(installation.id) {
        return Err(Failure::not_found());
    }
    let stored = store.credential(id).await?.ok_or_else(Failure::not_found)?;
    info!(account = %id, revision = stored.revision, "credential sent back to the desktop");
    Ok(Json(CredentialCopy {
        revision: stored.revision,
        credential: stored.credential,
    }))
}

async fn publications(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
) -> ApiResult<Vec<PublicationStatus>> {
    let runner = &state.runner;
    Ok(Json(
        runner
            .store
            .statuses(installation.id, runner.clock.now())
            .await?,
    ))
}

async fn publication(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Path(id): Path<PublicationId>,
) -> ApiResult<PublicationStatus> {
    state
        .runner
        .store
        .status(installation.id, id)
        .await?
        .map(Json)
        .ok_or_else(Failure::not_found)
}

async fn hand_over(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Path(id): Path<PublicationId>,
    Json(body): Json<HandoverPut>,
) -> ApiResult<PublicationStatus> {
    if body.publication.id != id {
        return Err(Failure::invalid(
            "the publication id doesn't match the address",
        ));
    }
    check_sha(&body.media.sha256)?;
    let runner = &state.runner;
    let received = runner
        .store
        .receive_handover(installation.id, &body, runner.clock.now())
        .await?;
    if let Received::New(_) = &received {
        info!(publication = %id, handover = %body.handover, "received a hand-over");
        runner.poke();
    }
    state
        .runner
        .store
        .status(installation.id, id)
        .await?
        .map(Json)
        .ok_or_else(Failure::not_found)
}

async fn release(
    State(state): State<AppState>,
    Extension(installation): Extension<Installation>,
    Path(id): Path<PublicationId>,
    Json(body): Json<ReleaseRequest>,
) -> ApiResult<PublicationStatus> {
    let runner = &state.runner;
    let status = runner
        .store
        .release(installation.id, id, body.handover, runner.clock.now())
        .await?;
    info!(publication = %id, state = %status.report.state, "given back to the desktop");
    Ok(Json(status))
}
