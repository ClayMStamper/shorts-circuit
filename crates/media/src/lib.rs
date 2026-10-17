//! FFprobe inspection, versioned render recipes, bounded FFmpeg rendering and cache.
//!
//! - [`tools`] finds the FFmpeg/FFprobe binaries (env override, bundled next to the app, PATH).
//! - [`probe`] reads technical properties with FFprobe.
//! - [`args`] turns a [`RenderRecipe`](shorts_domain::RenderRecipe) into FFmpeg arguments.
//! - [`render`] runs FFmpeg as a bounded subprocess (concurrency limit, timeout, kill on drop)
//!   and stores results in the [`cache`], keyed by source checksum + recipe + encoder version.
//!
//! Originals are only ever read. Renders and thumbnails are regenerable cache.

pub mod args;
pub mod cache;
pub mod checksum;
pub mod probe;
mod process;
pub mod render;
#[doc(hidden)]
pub mod testing;
pub mod thumbnail;
pub mod tools;

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

pub use cache::{CacheKey, RenderCache};
pub use checksum::sha256_file;
pub use probe::probe;
pub use render::{RenderRequest, RenderResult, Renderer};
pub use thumbnail::thumbnail;
pub use tools::MediaTools;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("{tool} was not found; install FFmpeg or set {env_var}")]
    ToolMissing {
        tool: &'static str,
        env_var: &'static str,
    },
    #[error("{0} does not exist")]
    MissingInput(PathBuf),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{tool} timed out after {after:?}")]
    Timeout { tool: &'static str, after: Duration },
    #[error("{tool} failed ({status}): {stderr}")]
    Failed {
        tool: &'static str,
        status: String,
        stderr: String,
    },
    #[error("could not read probe output: {0}")]
    Probe(String),
    #[error("the recipe is invalid: {0}")]
    InvalidRecipe(String),
}

impl MediaError {
    /// Worth retrying later (the machine was busy, a file was locked), as opposed to a bad
    /// input or recipe.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Timeout { .. } | Self::Io(_))
    }
}

pub type MediaResult<T> = Result<T, MediaError>;
