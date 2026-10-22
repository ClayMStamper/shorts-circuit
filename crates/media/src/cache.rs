//! Render cache. Renders are regenerable: deleting the cache directory is always safe.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use shorts_domain::{Checksum, RenderRecipe};

use crate::MediaResult;
use crate::args::PIPELINE_VERSION;

/// Identifies one render: hash of source checksum, recipe, encoder version, pipeline version
/// and the checksums of any side inputs (subtitle file, replacement audio).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn compute(
        source: &Checksum,
        recipe: &RenderRecipe,
        encoder_version: &str,
        side_inputs: &[Checksum],
    ) -> Self {
        let mut h = Sha256::new();
        h.update(b"shorts-circuit render\n");
        h.update(format!("pipeline {PIPELINE_VERSION}\n").as_bytes());
        h.update(format!("encoder {encoder_version}\n").as_bytes());
        h.update(format!("source {source}\n").as_bytes());
        h.update(format!("recipe {}\n", recipe.canonical_json()).as_bytes());
        for side in side_inputs {
            h.update(format!("side {side}\n").as_bytes());
        }
        Self(hex::encode(h.finalize()))
    }

    pub fn from_string(key: String) -> Self {
        Self(key)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct RenderCache {
    root: PathBuf,
}

impl RenderCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, key: &CacheKey) -> PathBuf {
        self.root
            .join("renders")
            .join(&key.0[..2])
            .join(format!("{key}.mp4"))
    }

    pub fn lookup(&self, key: &CacheKey) -> Option<PathBuf> {
        let path = self.path_for(key);
        path.is_file().then_some(path)
    }

    /// A fresh temporary path next to the final location, so the commit is a same-volume
    /// rename and a half-written file is never visible under the final name.
    pub async fn temp_path(&self, key: &CacheKey) -> MediaResult<PathBuf> {
        let final_path = self.path_for(key);
        let dir = final_path.parent().expect("cache paths have a parent");
        tokio::fs::create_dir_all(dir).await?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        Ok(dir.join(format!("{key}.{}.{nanos}.partial", std::process::id())))
    }

    pub async fn commit(&self, temp: &Path, key: &CacheKey) -> MediaResult<PathBuf> {
        let final_path = self.path_for(key);
        tokio::fs::rename(temp, &final_path).await?;
        Ok(final_path)
    }

    pub async fn evict(&self, key: &CacheKey) -> MediaResult<()> {
        match tokio::fs::remove_file(self.path_for(key)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn thumbnail_path(&self, source: &Checksum, at_ms: u64) -> PathBuf {
        self.root
            .join("thumbnails")
            .join(format!("{source}-{at_ms}.jpg"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shorts_domain::AudioChoice;

    fn sum(c: char) -> Checksum {
        Checksum::parse(&c.to_string().repeat(64)).unwrap()
    }

    #[test]
    fn keys_change_with_every_input() {
        let recipe = RenderRecipe::default();
        let base = CacheKey::compute(&sum('a'), &recipe, "ffmpeg 7.1", &[]);
        assert_eq!(
            base,
            CacheKey::compute(&sum('a'), &recipe, "ffmpeg 7.1", &[])
        );
        assert_ne!(
            base,
            CacheKey::compute(&sum('b'), &recipe, "ffmpeg 7.1", &[])
        );
        assert_ne!(
            base,
            CacheKey::compute(&sum('a'), &recipe, "ffmpeg 7.2", &[])
        );
        assert_ne!(
            base,
            CacheKey::compute(&sum('a'), &recipe, "ffmpeg 7.1", &[sum('c')])
        );
        let muted = RenderRecipe {
            audio: AudioChoice::Mute,
            ..Default::default()
        };
        assert_ne!(
            base,
            CacheKey::compute(&sum('a'), &muted, "ffmpeg 7.1", &[])
        );
    }

    #[tokio::test]
    async fn commit_is_a_rename_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RenderCache::new(dir.path());
        let key = CacheKey::compute(&sum('a'), &RenderRecipe::default(), "v", &[]);
        assert!(cache.lookup(&key).is_none());
        let tmp = cache.temp_path(&key).await.unwrap();
        tokio::fs::write(&tmp, b"video").await.unwrap();
        assert!(cache.lookup(&key).is_none(), "partial files are invisible");
        let path = cache.commit(&tmp, &key).await.unwrap();
        assert_eq!(cache.lookup(&key), Some(path));
        cache.evict(&key).await.unwrap();
        cache.evict(&key).await.unwrap();
        assert!(cache.lookup(&key).is_none());
    }
}
