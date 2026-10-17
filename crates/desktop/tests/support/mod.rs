//! Shared fixtures for desktop workflow tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use shorts_adapters::AdapterRegistry;
use shorts_desktop::{App, AppPaths, MediaBackend, RenderOutcome};
use shorts_domain::{MediaFile, MediaProperties, RenderRecipe};
use shorts_media::MediaError;
use shorts_scheduler::{ManualClock, RetryPolicy, WorkerConfig};
use tempfile::TempDir;

pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// Pretends to be FFmpeg. Files whose content starts with `bad` don't probe.
#[derive(Default)]
pub struct FakeMedia {
    pub renders: AtomicUsize,
    pub cache: PathBuf,
}

#[async_trait]
impl MediaBackend for FakeMedia {
    async fn probe(&self, path: &Path) -> Result<MediaProperties, MediaError> {
        let bytes = std::fs::read(path)?;
        if bytes.starts_with(b"bad") {
            return Err(MediaError::Probe("no video stream".into()));
        }
        Ok(MediaProperties {
            duration_ms: 20_000,
            width: 1920,
            height: 1080,
            audio_streams: 1,
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            ..Default::default()
        })
    }

    async fn render(
        &self,
        source: &MediaFile,
        recipe: &RenderRecipe,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<RenderOutcome, MediaError> {
        let n = self.renders.fetch_add(1, Ordering::SeqCst);
        std::fs::create_dir_all(&self.cache)?;
        let path = self.cache.join(format!("render-{n}.mp4"));
        std::fs::copy(&source.path, &path)?;
        progress(0.5);
        progress(1.0);
        Ok(RenderOutcome {
            size_bytes: std::fs::metadata(&path)?.len(),
            path,
            cache_key: format!("key-{n}"),
            properties: MediaProperties {
                duration_ms: recipe.output_duration_ms(source.properties.duration_ms),
                width: recipe.encoding.width,
                height: recipe.encoding.height,
                audio_streams: u32::from(recipe.produces_audio(&source.properties)),
                video_codec: Some("h264".into()),
                ..Default::default()
            },
        })
    }

    async fn thumbnail(&self, source: &MediaFile, _at_ms: u64) -> Result<PathBuf, MediaError> {
        std::fs::create_dir_all(&self.cache)?;
        let path = self.cache.join(format!("{}.jpg", source.checksum));
        std::fs::write(&path, [0xFF, 0xD8, 0xFF])?;
        Ok(path)
    }
}

pub struct Harness {
    pub dir: TempDir,
    pub app: Arc<App>,
    pub clock: ManualClock,
    pub media: Arc<FakeMedia>,
}

pub async fn harness() -> Harness {
    harness_with(AdapterRegistry::new()).await
}

pub async fn harness_with(adapters: AdapterRegistry) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let media = Arc::new(FakeMedia {
        cache: dir.path().join("data").join("cache"),
        ..Default::default()
    });
    let clock = ManualClock::new(t0());
    let app = App::open(
        AppPaths::under(&dir.path().join("data")),
        media.clone(),
        adapters,
        Arc::new(clock.clone()),
    )
    .await
    .unwrap();
    Harness {
        dir,
        app,
        clock,
        media,
    }
}

pub fn worker_config() -> WorkerConfig {
    WorkerConfig {
        poll_interval: Duration::from_millis(20),
        retry: RetryPolicy {
            base: Duration::from_secs(10),
            max: Duration::from_secs(300),
            max_failures: 6,
        },
        concurrency: 1,
        ..WorkerConfig::default()
    }
}

impl Harness {
    /// Writes a fake video file to import.
    pub fn video(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self.dir.path().join("incoming");
        std::fs::create_dir_all(&path).unwrap();
        let path = path.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Runs due jobs until none are left.
    pub async fn drain(&self) -> Vec<shorts_scheduler::Disposition> {
        let worker = self.app.worker(worker_config()).await.unwrap();
        let mut out = Vec::new();
        while let Some((_, d)) = worker.run_once().await.unwrap() {
            out.push(d);
            assert!(out.len() < 50, "runaway jobs: {out:?}");
        }
        out
    }
}
