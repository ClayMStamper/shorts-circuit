//! What the runner needs to publish as an account: the same secrets the desktop keeps in the
//! OS credential store, in a platform-tagged form. Secrets redact themselves in `Debug`.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shorts_domain::Platform;

/// A secret string. `Debug` never shows it; read it with [`Secret::expose`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// One account's credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum CloudCredential {
    /// The owner's Desktop-app OAuth client and the channel's refresh token.
    YoutubeShorts {
        client_id: String,
        client_secret: Secret,
        refresh_token: Secret,
        channel_id: String,
    },
    /// A long-lived token, renewable once a day old.
    InstagramReels {
        access_token: Secret,
        user_id: String,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    },
    /// A 60-day token for a Company Page; not refreshable.
    Linkedin {
        access_token: Secret,
        /// `urn:li:organization:<id>`.
        organization: String,
        expires_at: DateTime<Utc>,
    },
    /// The owner's Snap OAuth app and the profile's refresh token (Snap rotates it).
    SnapchatSpotlight {
        client_id: String,
        client_secret: Secret,
        refresh_token: Secret,
        profile_id: String,
    },
}

impl CloudCredential {
    pub fn platform(&self) -> Platform {
        match self {
            Self::YoutubeShorts { .. } => Platform::YoutubeShorts,
            Self::InstagramReels { .. } => Platform::InstagramReels,
            Self::Linkedin { .. } => Platform::Linkedin,
            Self::SnapchatSpotlight { .. } => Platform::SnapchatSpotlight,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_redacts_secrets() {
        let c = CloudCredential::YoutubeShorts {
            client_id: "client.apps.googleusercontent.com".into(),
            client_secret: Secret::new("fake-client-secret"),
            refresh_token: Secret::new("fake-refresh-token"),
            channel_id: "UCfake".into(),
        };
        let shown = format!("{c:?}");
        assert!(!shown.contains("fake-client-secret"));
        assert!(!shown.contains("fake-refresh-token"));
        assert!(shown.contains("<redacted>"));
        // The wire form carries them.
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("fake-refresh-token"));
        assert!(json.contains(r#""platform":"youtube_shorts""#));
        assert_eq!(serde_json::from_str::<CloudCredential>(&json).unwrap(), c);
        assert_eq!(c.platform(), Platform::YoutubeShorts);
    }
}
