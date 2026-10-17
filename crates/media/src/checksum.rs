//! Content checksums for originals and side inputs.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};
use shorts_domain::Checksum;

use crate::{MediaError, MediaResult};

/// SHA-256 of a file, computed on a blocking thread.
pub async fn sha256_file(path: &Path) -> MediaResult<(Checksum, u64)> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MediaError::MissingInput(path.clone())
            } else {
                MediaError::Io(e)
            }
        })?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            hasher.update(&buf[..n]);
        }
        let hex = hex::encode(hasher.finalize());
        Ok((Checksum::parse(&hex).expect("sha256 hex is valid"), total))
    })
    .await
    .map_err(|e| MediaError::Io(std::io::Error::other(e)))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashes_known_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.txt");
        std::fs::write(&path, b"abc").unwrap();
        let (sum, size) = sha256_file(&path).await.unwrap();
        assert_eq!(
            sum.as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(size, 3);
    }

    #[tokio::test]
    async fn missing_files_are_reported() {
        let err = sha256_file(Path::new("definitely/not/here.mp4"))
            .await
            .unwrap_err();
        assert!(matches!(err, MediaError::MissingInput(_)));
    }
}
