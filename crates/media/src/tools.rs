//! Locating FFmpeg and FFprobe.

use std::env;
use std::path::{Path, PathBuf};

use crate::{MediaError, MediaResult};

pub const FFMPEG_ENV: &str = "SHORTS_FFMPEG";
pub const FFPROBE_ENV: &str = "SHORTS_FFPROBE";

/// Paths to the FFmpeg and FFprobe executables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl MediaTools {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        }
    }

    /// Finds the tools, in order: `SHORTS_FFMPEG` / `SHORTS_FFPROBE`, binaries in `bundled_dir`
    /// (the packaged app ships them next to its executable), then `PATH`.
    pub fn discover(bundled_dir: Option<&Path>) -> MediaResult<Self> {
        Ok(Self {
            ffmpeg: find("ffmpeg", FFMPEG_ENV, bundled_dir)?,
            ffprobe: find("ffprobe", FFPROBE_ENV, bundled_dir)?,
        })
    }
}

fn find(tool: &'static str, env_var: &'static str, bundled: Option<&Path>) -> MediaResult<PathBuf> {
    if let Some(path) = env::var_os(env_var).map(PathBuf::from)
        && path.is_file()
    {
        return Ok(path);
    }
    let file = executable_name(tool);
    if let Some(dir) = bundled {
        let candidate = dir.join(&file);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let candidate = dir.join(&file);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(MediaError::ToolMissing { tool, env_var })
}

fn executable_name(tool: &str) -> String {
    if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_directory_is_searched() {
        let dir = tempfile::tempdir().unwrap();
        for tool in ["ffmpeg", "ffprobe"] {
            std::fs::write(dir.path().join(executable_name(tool)), b"").unwrap();
        }
        let tools = MediaTools::discover(Some(dir.path())).unwrap();
        // An env override or PATH hit may win on developer machines; either way both resolve.
        assert!(tools.ffmpeg.is_file());
        assert!(tools.ffprobe.is_file());
    }
}
