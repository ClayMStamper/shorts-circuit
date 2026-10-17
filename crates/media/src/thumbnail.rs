//! Poster frames for the library grid.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use shorts_domain::Checksum;

use crate::cache::RenderCache;
use crate::tools::MediaTools;
use crate::{MediaResult, process};

/// A 360-pixel-wide JPEG of the frame at `at_ms`, cached per source checksum.
pub async fn thumbnail(
    tools: &MediaTools,
    cache: &RenderCache,
    source: &Path,
    checksum: &Checksum,
    at_ms: u64,
) -> MediaResult<PathBuf> {
    let path = cache.thumbnail_path(checksum, at_ms);
    if path.is_file() {
        return Ok(path);
    }
    let dir = path.parent().expect("thumbnail paths have a parent");
    tokio::fs::create_dir_all(dir).await?;
    let temp = dir.join(format!("{checksum}-{at_ms}.{}.partial", std::process::id()));
    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{}.{:03}", at_ms / 1000, at_ms % 1000).into(),
        "-i".into(),
        source.as_os_str().to_owned(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        "scale=360:-2".into(),
        "-q:v".into(),
        "4".into(),
        "-f".into(),
        "image2".into(),
        temp.as_os_str().to_owned(),
    ];
    if let Err(e) = process::run(
        "ffmpeg",
        &tools.ffmpeg,
        &args,
        Duration::from_secs(60),
        |_| {},
    )
    .await
    {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(e);
    }
    tokio::fs::rename(&temp, &path).await?;
    Ok(path)
}
