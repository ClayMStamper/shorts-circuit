//! Bounded FFmpeg rendering into the cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use shorts_domain::{AudioChoice, Checksum, MediaProperties, RenderRecipe};
use tokio::sync::{OnceCell, Semaphore};
use tracing::{debug, info};

use crate::args::render_args;
use crate::cache::{CacheKey, RenderCache};
use crate::checksum::sha256_file;
use crate::tools::MediaTools;
use crate::{MediaError, MediaResult, probe, process};

pub struct RenderRequest<'a> {
    pub source_path: &'a Path,
    pub source_checksum: &'a Checksum,
    pub source: &'a MediaProperties,
    pub recipe: &'a RenderRecipe,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderResult {
    pub path: PathBuf,
    pub cache_key: CacheKey,
    pub size_bytes: u64,
    pub properties: MediaProperties,
    pub from_cache: bool,
}

/// Renders recipes with at most `max_parallel` FFmpeg processes at a time.
pub struct Renderer {
    tools: MediaTools,
    cache: RenderCache,
    permits: Arc<Semaphore>,
    encoder_version: OnceCell<String>,
}

impl Renderer {
    pub fn new(tools: MediaTools, cache: RenderCache, max_parallel: usize) -> Self {
        Self {
            tools,
            cache,
            permits: Arc::new(Semaphore::new(max_parallel.max(1))),
            encoder_version: OnceCell::new(),
        }
    }

    pub fn tools(&self) -> &MediaTools {
        &self.tools
    }

    pub fn cache(&self) -> &RenderCache {
        &self.cache
    }

    /// The first line of `ffmpeg -version`, without the copyright notice.
    pub async fn encoder_version(&self) -> MediaResult<String> {
        self.encoder_version
            .get_or_try_init(|| async {
                let out = process::run(
                    "ffmpeg",
                    &self.tools.ffmpeg,
                    &["-hide_banner".into(), "-version".into()],
                    Duration::from_secs(30),
                    |_| {},
                )
                .await?;
                let text = String::from_utf8_lossy(&out.stdout);
                let first = text.lines().next().unwrap_or("ffmpeg unknown");
                Ok(first
                    .split(" Copyright")
                    .next()
                    .unwrap_or(first)
                    .trim()
                    .to_owned())
            })
            .await
            .cloned()
    }

    pub async fn cache_key(&self, req: &RenderRequest<'_>) -> MediaResult<CacheKey> {
        let mut sides = Vec::new();
        if let Some(subs) = &req.recipe.subtitles {
            sides.push(sha256_file(Path::new(&subs.path)).await?.0);
        }
        if let AudioChoice::Replace { path } = &req.recipe.audio {
            sides.push(sha256_file(Path::new(path)).await?.0);
        }
        let encoder = self.encoder_version().await?;
        Ok(CacheKey::compute(
            req.source_checksum,
            req.recipe,
            &encoder,
            &sides,
        ))
    }

    /// Returns the cached render or produces it. `progress` receives values in `0.0..=1.0`.
    pub async fn render(
        &self,
        req: &RenderRequest<'_>,
        progress: impl Fn(f64) + Send + Sync,
    ) -> MediaResult<RenderResult> {
        let issues = req.recipe.validate(Some(req.source));
        if !issues.is_empty() {
            return Err(MediaError::InvalidRecipe(format!("{issues:?}")));
        }
        if !req.source_path.is_file() {
            return Err(MediaError::MissingInput(req.source_path.to_owned()));
        }
        let key = self.cache_key(req).await?;
        if let Some(path) = self.cache.lookup(&key) {
            debug!(%key, "render cache hit");
            return self.result(path, key, true).await;
        }

        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|e| MediaError::Io(std::io::Error::other(e)))?;
        // Another job may have produced it while we waited.
        if let Some(path) = self.cache.lookup(&key) {
            return self.result(path, key, true).await;
        }

        let temp = self.cache.temp_path(&key).await?;
        let args = render_args(req.source_path, req.source, req.recipe, &temp);
        let out_ms = req.recipe.output_duration_ms(req.source.duration_ms).max(1);
        let timeout =
            Duration::from_millis(out_ms.saturating_mul(20)).max(Duration::from_secs(120));
        info!(%key, ?timeout, "rendering");
        let result = process::run("ffmpeg", &self.tools.ffmpeg, &args, timeout, |line| {
            if let Some(us) = line
                .strip_prefix("out_time_us=")
                .or_else(|| line.strip_prefix("out_time_ms="))
                .and_then(|v| v.trim().parse::<u64>().ok())
            {
                progress((us as f64 / 1000.0 / out_ms as f64).clamp(0.0, 1.0));
            }
        })
        .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(e);
        }
        progress(1.0);
        let path = self.cache.commit(&temp, &key).await?;
        self.result(path, key, false).await
    }

    async fn result(
        &self,
        path: PathBuf,
        key: CacheKey,
        from_cache: bool,
    ) -> MediaResult<RenderResult> {
        let size_bytes = tokio::fs::metadata(&path).await?.len();
        let properties = probe(&self.tools, &path).await?;
        Ok(RenderResult {
            path,
            cache_key: key,
            size_bytes,
            properties,
            from_cache,
        })
    }
}
