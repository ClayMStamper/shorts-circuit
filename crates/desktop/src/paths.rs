use std::path::{Path, PathBuf};

/// Where the app keeps its data. Originals are durable; `cache` is regenerable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub database: PathBuf,
    /// Managed, read-only copies of imported originals.
    pub originals: PathBuf,
    /// Renders and thumbnails.
    pub cache: PathBuf,
    /// Default location for export packages.
    pub exports: PathBuf,
}

impl AppPaths {
    pub fn under(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_owned(),
            database: data_dir.join("library.db"),
            originals: data_dir.join("originals"),
            cache: data_dir.join("cache"),
            exports: data_dir.join("exports"),
        }
    }

    pub fn create_all(&self) -> std::io::Result<()> {
        for dir in [&self.data_dir, &self.originals, &self.cache, &self.exports] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
