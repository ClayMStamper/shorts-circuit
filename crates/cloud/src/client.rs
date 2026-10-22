//! The HTTPS client for the runner's sync API, used by the desktop (and the runner's tests).

use std::fmt;
use std::path::Path;
use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use shorts_domain::{AccountId, HandoverId, PublicationId};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::credential::Secret;
use crate::protocol::{
    AccountPut, AccountStatus, ApiError, CredentialCopy, ErrorCode, HandoverPut, MAX_CHUNK_BYTES,
    PublicationStatus, ReleaseRequest, UploadStart, UploadStatus,
};

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ClientError {
    #[error("the cloud runner address is not usable: {0}")]
    Config(String),
    #[error("could not reach the cloud runner: {0}")]
    Transport(String),
    /// The runner answered with an error.
    #[error("the cloud runner refused: {}", .0.message)]
    Api(ApiError),
    #[error("unexpected answer from the cloud runner: {0}")]
    Protocol(String),
}

impl ClientError {
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Api(e) => Some(e.code),
            _ => None,
        }
    }

    /// Worth trying again later (network trouble or a runner-side failure).
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transport(_)) || self.code() == Some(ErrorCode::Internal)
    }
}

pub type ClientResult<T> = Result<T, ClientError>;

/// Checks a runner address: HTTPS, or plain HTTP only to this machine (for tests and SSH
/// tunnels). Trailing slashes are ignored.
pub fn parse_runner_url(raw: &str) -> ClientResult<Url> {
    let url = Url::parse(raw.trim().trim_end_matches('/'))
        .map_err(|e| ClientError::Config(format!("{raw:?} is not an address: {e}")))?;
    let host = url.host_str().unwrap_or_default();
    let loopback = host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => {
            return Err(ClientError::Config(
                "use an https:// address; the token and your credentials must not travel \
                 unencrypted"
                    .into(),
            ));
        }
        other => {
            return Err(ClientError::Config(format!(
                "unsupported scheme {other:?}; use https://"
            )));
        }
    }
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        return Err(ClientError::Config(
            "the address must not carry a query, fragment or user name".into(),
        ));
    }
    Ok(url)
}

/// Lowercase hex SHA-256 of a file.
pub async fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Talks to one runner as one installation.
#[derive(Clone)]
pub struct CloudClient {
    http: reqwest::Client,
    base: Url,
    token: Secret,
    chunk_bytes: u64,
}

impl fmt::Debug for CloudClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudClient")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl CloudClient {
    pub fn new(base_url: &str, token: &str) -> ClientResult<Self> {
        let base = parse_runner_url(base_url)?;
        let token = token.trim();
        if token.is_empty() || token.chars().any(char::is_whitespace) {
            return Err(ClientError::Config(
                "paste the installation token the runner printed".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ClientError::Config(e.to_string()))?;
        Ok(Self {
            http,
            base,
            token: Secret::new(token),
            chunk_bytes: MAX_CHUNK_BYTES,
        })
    }

    /// Smaller upload chunks (tests).
    pub fn with_chunk_bytes(mut self, bytes: u64) -> Self {
        self.chunk_bytes = bytes.clamp(1, MAX_CHUNK_BYTES);
        self
    }

    pub fn base_url(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}{path}"));
        url
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, self.url(path))
            .bearer_auth(self.token.expose())
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> ClientResult<T> {
        let response = request.send().await.map_err(|e| transport(&e))?;
        read(response).await
    }

    async fn send_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> ClientResult<T> {
        self.send(self.request(method, path).json(body)).await
    }

    /// Checks the address and the token.
    pub async fn check(&self) -> ClientResult<()> {
        let _: Vec<AccountStatus> = self.accounts().await?;
        Ok(())
    }

    /// Uploads `path` (whose SHA-256 is `sha256`), resuming wherever the runner already is.
    /// `progress` is called with the fraction received after each chunk.
    pub async fn upload_file(
        &self,
        path: &Path,
        sha256: &str,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> ClientResult<UploadStatus> {
        let size_bytes = tokio::fs::metadata(path)
            .await
            .map_err(|e| ClientError::Config(format!("cannot read the video: {e}")))?
            .len();
        let mut status: UploadStatus = self
            .send_json(
                Method::POST,
                "/v1/uploads",
                &UploadStart {
                    sha256: sha256.to_owned(),
                    size_bytes,
                },
            )
            .await?;
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| ClientError::Config(format!("cannot read the video: {e}")))?;
        let mut realigned = 0;
        while !status.complete {
            let offset = status.received_bytes;
            let len = self.chunk_bytes.min(size_bytes.saturating_sub(offset));
            let mut chunk = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX)];
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| ClientError::Config(format!("cannot read the video: {e}")))?;
            file.read_exact(&mut chunk)
                .await
                .map_err(|e| ClientError::Config(format!("cannot read the video: {e}")))?;
            let request = self
                .request(Method::PUT, &format!("/v1/uploads/{sha256}"))
                .query(&[("offset", offset)])
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(chunk);
            match self.send::<UploadStatus>(request).await {
                Ok(next) => {
                    status = next;
                    if size_bytes > 0 {
                        progress(status.received_bytes as f64 / size_bytes as f64);
                    }
                }
                // Another attempt got further (or a chunk landed without its answer).
                Err(ClientError::Api(ApiError {
                    code: ErrorCode::OffsetMismatch,
                    received_bytes: Some(at),
                    ..
                })) if realigned < 3 => {
                    realigned += 1;
                    status.received_bytes = at;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(status)
    }

    pub async fn upload_status(&self, sha256: &str) -> ClientResult<UploadStatus> {
        self.send(self.request(Method::GET, &format!("/v1/uploads/{sha256}")))
            .await
    }

    pub async fn put_account(&self, put: &AccountPut) -> ClientResult<AccountStatus> {
        self.send_json(
            Method::PUT,
            &format!("/v1/accounts/{}", put.account.id),
            put,
        )
        .await
    }

    pub async fn accounts(&self) -> ClientResult<Vec<AccountStatus>> {
        self.send(self.request(Method::GET, "/v1/accounts")).await
    }

    pub async fn credential(&self, account: AccountId) -> ClientResult<CredentialCopy> {
        self.send(self.request(Method::GET, &format!("/v1/accounts/{account}/credential")))
            .await
    }

    pub async fn hand_over(&self, put: &HandoverPut) -> ClientResult<PublicationStatus> {
        self.send_json(
            Method::PUT,
            &format!("/v1/publications/{}", put.publication.id),
            put,
        )
        .await
    }

    pub async fn publications(&self) -> ClientResult<Vec<PublicationStatus>> {
        self.send(self.request(Method::GET, "/v1/publications"))
            .await
    }

    pub async fn publication(&self, id: PublicationId) -> ClientResult<PublicationStatus> {
        self.send(self.request(Method::GET, &format!("/v1/publications/{id}")))
            .await
    }

    /// Takes the publication back. `not_found` means the runner never received it.
    pub async fn release(
        &self,
        id: PublicationId,
        handover: HandoverId,
    ) -> ClientResult<PublicationStatus> {
        self.send_json(
            Method::POST,
            &format!("/v1/publications/{id}/release"),
            &ReleaseRequest { handover },
        )
        .await
    }
}

fn transport(e: &reqwest::Error) -> ClientError {
    // The URL can't carry secrets (checked in `parse_runner_url`), but keep messages short.
    ClientError::Transport(if e.is_timeout() {
        "timed out".to_owned()
    } else if e.is_connect() {
        "could not connect".to_owned()
    } else {
        "the connection broke".to_owned()
    })
}

async fn read<T: DeserializeOwned>(response: Response) -> ClientResult<T> {
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.to_string()));
    }
    let body = response.bytes().await.unwrap_or_default();
    Err(match serde_json::from_slice::<ApiError>(&body) {
        Ok(api) => ClientError::Api(api),
        Err(_) => ClientError::Api(ApiError {
            code: match status {
                StatusCode::UNAUTHORIZED => ErrorCode::Unauthorized,
                StatusCode::NOT_FOUND => ErrorCode::NotFound,
                StatusCode::CONFLICT => ErrorCode::Conflict,
                StatusCode::GONE => ErrorCode::Gone,
                s if s.is_client_error() => ErrorCode::Invalid,
                _ => ErrorCode::Internal,
            },
            message: format!("HTTP {}", status.as_u16()),
            received_bytes: None,
            revision: None,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_addresses_must_be_https_except_on_this_machine() {
        assert!(parse_runner_url("https://runner.example.com").is_ok());
        assert!(parse_runner_url("https://runner.example.com/base/").is_ok());
        assert!(parse_runner_url("http://127.0.0.1:8787").is_ok());
        assert!(parse_runner_url("http://localhost:8787").is_ok());
        assert!(parse_runner_url("http://[::1]:8787").is_ok());
        assert!(parse_runner_url("http://runner.example.com").is_err());
        assert!(parse_runner_url("ftp://runner.example.com").is_err());
        assert!(parse_runner_url("https://user@runner.example.com").is_err());
        assert!(parse_runner_url("https://runner.example.com/?t=1").is_err());
        assert!(parse_runner_url("not a url").is_err());
    }

    #[test]
    fn tokens_are_required_and_hidden() {
        assert!(CloudClient::new("https://runner.example.com", " ").is_err());
        let c = CloudClient::new("https://runner.example.com/", "sc_faketoken").unwrap();
        assert!(!format!("{c:?}").contains("sc_faketoken"));
        assert_eq!(
            c.url("/v1/uploads").as_str(),
            "https://runner.example.com/v1/uploads"
        );
    }

    #[tokio::test]
    async fn files_hash_to_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).await.unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
