//! Uploaded media on disk, named by SHA-256 under the data directory:
//! `uploads/<installation>/<sha256>.part` while it arrives, `media/<installation>/<sha256>`
//! once verified. Media is a copy of a desktop render, so it is deleted once no cloud-owned,
//! unpublished publication needs it.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

/// Checks that `sha256` is 64 lowercase hex characters, so it is safe in a file name.
pub fn valid_sha256(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone)]
pub struct MediaDir {
    root: PathBuf,
}

impl MediaDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn partial(&self, installation: Uuid, sha256: &str) -> PathBuf {
        self.root
            .join("uploads")
            .join(installation.to_string())
            .join(format!("{sha256}.part"))
    }

    /// Where a verified upload lives.
    pub fn path(&self, installation: Uuid, sha256: &str) -> PathBuf {
        self.root
            .join("media")
            .join(installation.to_string())
            .join(sha256)
    }

    /// Writes `bytes` at `offset` of the partial file, dropping anything after `offset` first
    /// (a chunk written before an interruption, whose answer never got recorded).
    pub async fn write_chunk(
        &self,
        installation: Uuid,
        sha256: &str,
        offset: u64,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let path = self.partial(installation, sha256);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .await?;
        let len = file.metadata().await?.len();
        if len < offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the partial upload is shorter than its recorded size",
            ));
        }
        file.set_len(offset).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(bytes).await?;
        file.sync_data().await?;
        Ok(())
    }

    /// Checks the partial file against `sha256` and moves it into place. Returns false (and
    /// deletes the partial file) on a mismatch.
    pub async fn finish(&self, installation: Uuid, sha256: &str) -> std::io::Result<bool> {
        let partial = self.partial(installation, sha256);
        if !partial.exists() {
            // An empty file never had a chunk written.
            if let Some(dir) = partial.parent() {
                tokio::fs::create_dir_all(dir).await?;
            }
            tokio::fs::write(&partial, b"").await?;
        }
        if hash(&partial).await? != sha256 {
            tokio::fs::remove_file(&partial).await?;
            return Ok(false);
        }
        let target = self.path(installation, sha256);
        if let Some(dir) = target.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::rename(&partial, &target).await?;
        Ok(true)
    }

    /// Deletes an upload's files, finished or not.
    pub async fn delete(&self, installation: Uuid, sha256: &str) -> std::io::Result<()> {
        for path in [
            self.partial(installation, sha256),
            self.path(installation, sha256),
        ] {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

async fn hash(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn only_hex_digests_name_files() {
        assert!(valid_sha256(ABC));
        assert!(!valid_sha256("../../etc/passwd"));
        assert!(!valid_sha256(&ABC.to_uppercase()));
        assert!(!valid_sha256(&ABC[..63]));
    }

    #[tokio::test]
    async fn chunks_resume_and_rewrite_unrecorded_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let media = MediaDir::new(dir.path());
        let inst = Uuid::now_v7();
        media.write_chunk(inst, ABC, 0, b"a").await.unwrap();
        // A chunk landed but its answer was lost; the client resends from offset 1.
        media.write_chunk(inst, ABC, 1, b"bX").await.unwrap();
        media.write_chunk(inst, ABC, 1, b"bc").await.unwrap();
        assert!(media.finish(inst, ABC).await.unwrap());
        assert_eq!(std::fs::read(media.path(inst, ABC)).unwrap(), b"abc");
        // Offsets past the end are refused.
        assert!(media.write_chunk(inst, ABC, 5, b"x").await.is_err());
    }

    #[tokio::test]
    async fn mismatched_files_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let media = MediaDir::new(dir.path());
        let inst = Uuid::now_v7();
        media.write_chunk(inst, ABC, 0, b"abd").await.unwrap();
        assert!(!media.finish(inst, ABC).await.unwrap());
        assert!(!media.path(inst, ABC).exists());
        media.delete(inst, ABC).await.unwrap();
    }
}
