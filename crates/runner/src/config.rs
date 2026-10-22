//! Process configuration from the environment. Secrets (the database URL may carry a
//! password; the master key) never appear in `Debug` output or logs.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

pub const DATABASE_URL_ENV: &str = "DATABASE_URL";
pub const DATA_DIR_ENV: &str = "SHORTS_RUNNER_DATA_DIR";
pub const BIND_ENV: &str = "SHORTS_RUNNER_BIND";

pub struct RunnerConfig {
    pub database_url: String,
    /// Uploaded media lives under here.
    pub data_dir: PathBuf,
    /// Plain HTTP, normally on loopback behind a TLS-terminating reverse proxy.
    pub bind: SocketAddr,
}

impl fmt::Debug for RunnerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerConfig")
            .field("database_url", &"<redacted>")
            .field("data_dir", &self.data_dir)
            .field("bind", &self.bind)
            .finish()
    }
}

impl RunnerConfig {
    /// `DATABASE_URL` (required), `SHORTS_RUNNER_DATA_DIR` (default `/var/lib/shorts-runner`)
    /// and `SHORTS_RUNNER_BIND` (default `127.0.0.1:8787`).
    pub fn from_env() -> Result<Self, String> {
        let var = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let database_url =
            var(DATABASE_URL_ENV).ok_or_else(|| format!("{DATABASE_URL_ENV} is not set"))?;
        let data_dir = var(DATA_DIR_ENV)
            .map_or_else(|| PathBuf::from("/var/lib/shorts-runner"), PathBuf::from);
        let bind = var(BIND_ENV)
            .unwrap_or_else(|| "127.0.0.1:8787".to_owned())
            .parse()
            .map_err(|e| format!("{BIND_ENV}: {e}"))?;
        Ok(Self {
            database_url,
            data_dir,
            bind,
        })
    }
}
