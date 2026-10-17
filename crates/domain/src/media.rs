//! Original media files and their technical properties.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{AssetId, MediaFileId};

#[derive(Debug, Error, PartialEq, Eq)]
#[error("checksum must be 64 lowercase hex characters (SHA-256)")]
pub struct InvalidChecksum;

/// SHA-256 of a file's bytes, lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Checksum(String);

impl Checksum {
    pub fn parse(hex: &str) -> Result<Self, InvalidChecksum> {
        let valid = hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if valid {
            Ok(Self(hex.to_owned()))
        } else {
            Err(InvalidChecksum)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Checksum {
    type Error = InvalidChecksum;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Checksum> for String {
    fn from(c: Checksum) -> Self {
        c.0
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Technical properties reported by FFprobe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MediaProperties {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// Rotation from display-matrix side data, in degrees (0, 90, 180, 270).
    pub rotation: i32,
    pub frame_rate: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_streams: u32,
    pub container: Option<String>,
    pub bit_rate: Option<u64>,
}

impl MediaProperties {
    /// Width and height as shown to the viewer, accounting for rotation.
    pub fn display_dimensions(&self) -> (u32, u32) {
        if self.rotation.rem_euclid(180) == 90 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    pub fn is_vertical(&self) -> bool {
        let (w, h) = self.display_dimensions();
        h > w
    }

    pub fn has_audio(&self) -> bool {
        self.audio_streams > 0
    }
}

/// An original file. Its bytes are immutable; the checksum identifies them and detects
/// duplicate imports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaFile {
    pub id: MediaFileId,
    pub asset_id: AssetId,
    /// Where the managed copy lives.
    pub path: String,
    /// The file name it was imported from, for display.
    pub original_name: String,
    pub checksum: Checksum,
    pub size_bytes: u64,
    pub properties: MediaProperties,
    pub imported_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_must_be_sha256_hex() {
        assert!(Checksum::parse(&"a".repeat(64)).is_ok());
        assert!(Checksum::parse(&"A".repeat(64)).is_err());
        assert!(Checksum::parse("abc").is_err());
        assert!(Checksum::parse(&"g".repeat(64)).is_err());
    }

    #[test]
    fn rotation_swaps_display_dimensions() {
        let props = MediaProperties {
            width: 1920,
            height: 1080,
            rotation: -90,
            ..Default::default()
        };
        assert_eq!(props.display_dimensions(), (1080, 1920));
        assert!(props.is_vertical());
    }
}
