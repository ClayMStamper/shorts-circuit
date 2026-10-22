//! The media operations the app needs, behind a trait so tests can run without FFmpeg.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use shorts_domain::{Checksum, MediaFile, MediaProperties, RenderRecipe};
use shorts_media::{MediaError, RenderCache, RenderRequest, Renderer, sha256_file};

#[derive(Debug, Clone, PartialEq)]
pub struct RenderOutcome {
    pub path: PathBuf,
    pub cache_key: String,
    pub size_bytes: u64,
    pub properties: MediaProperties,
}

#[async_trait]
pub trait MediaBackend: Send + Sync + 'static {
    async fn checksum(&self, path: &Path) -> Result<(Checksum, u64), MediaError> {
        sha256_file(path).await
    }

    async fn probe(&self, path: &Path) -> Result<MediaProperties, MediaError>;

    async fn render(
        &self,
        source: &MediaFile,
        recipe: &RenderRecipe,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<RenderOutcome, MediaError>;

    async fn thumbnail(&self, source: &MediaFile, at_ms: u64) -> Result<PathBuf, MediaError>;
}

/// The real backend: FFprobe and FFmpeg subprocesses with the render cache.
pub struct FfmpegBackend {
    renderer: Renderer,
}

impl FfmpegBackend {
    pub fn new(renderer: Renderer) -> Self {
        Self { renderer }
    }

    pub fn cache(&self) -> &RenderCache {
        self.renderer.cache()
    }
}

#[async_trait]
impl MediaBackend for FfmpegBackend {
    async fn probe(&self, path: &Path) -> Result<MediaProperties, MediaError> {
        shorts_media::probe(self.renderer.tools(), path).await
    }

    async fn render(
        &self,
        source: &MediaFile,
        recipe: &RenderRecipe,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<RenderOutcome, MediaError> {
        let result = self
            .renderer
            .render(
                &RenderRequest {
                    source_path: Path::new(&source.path),
                    source_checksum: &source.checksum,
                    source: &source.properties,
                    recipe,
                },
                progress,
            )
            .await?;
        Ok(RenderOutcome {
            path: result.path,
            cache_key: result.cache_key.to_string(),
            size_bytes: result.size_bytes,
            properties: result.properties,
        })
    }

    async fn thumbnail(&self, source: &MediaFile, at_ms: u64) -> Result<PathBuf, MediaError> {
        shorts_media::thumbnail(
            self.renderer.tools(),
            self.renderer.cache(),
            Path::new(&source.path),
            &source.checksum,
            at_ms,
        )
        .await
    }
}
