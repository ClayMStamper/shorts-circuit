//! The optional cloud runner. It executes approved publications that the desktop app handed
//! over, while the owner's computer is off, using the same domain logic, scheduler, publish
//! pipeline and adapters as the desktop, against PostgreSQL.
//!
//! - [`db`]: PostgreSQL persistence (sqlx migrations), with execution ownership enforced when
//!   claiming and releasing jobs.
//! - [`vault`]: envelope encryption of credentials at rest under a master key from the
//!   environment.
//! - [`tokens`]: adapter sessions from those credentials, refreshed like the desktop's.
//! - [`api`]: the HTTPS sync API the desktop talks to (behind a TLS-terminating proxy).
//! - [`runner::Runner`]: wires them together with the durable worker.

pub mod api;
pub mod config;
pub mod db;
pub mod host;
pub mod media;
pub mod runner;
pub mod tokens;
pub mod vault;

pub use config::RunnerConfig;
pub use db::{DbError, DbResult, PgStore};
pub use media::MediaDir;
pub use runner::{Runner, RunnerSetup};
pub use vault::Vault;
