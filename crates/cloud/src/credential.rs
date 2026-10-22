//! What the runner needs to publish as an account: the same secrets the desktop keeps in the
//! OS credential store, in a platform-tagged form. Secrets redact themselves in `Debug`.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shorts_domain::Platform;
use shorts_domain::instagram::InstagramLogin;

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
    /// With Facebook login, the linked Page's token (it doesn't expire) and the Instagram
    /// account id. Older credentials (Instagram Login, no `login` field) can't publish.
    InstagramReels {
        access_token: Secret,
        user_id: String,
        #[serde(default)]
        login: InstagramLogin,
        #[serde(default)]
        expires_at: Option<DateTime<Utc>>,
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
    /// A SHA-256 over the whole credential, so the desktop can tell whether it changed since
    /// it last sent it without keeping a copy.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self).expect("credentials serialize");
        hex::encode(Sha256::digest(bytes))
    }

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

    #[test]
    fn instagram_credentials_from_before_facebook_login_still_read() {
        let old = r#"{"platform":"instagram_reels","access_token":"fake-igaa","user_id":"1",
            "issued_at":"2026-10-01T00:00:00Z","expires_at":"2026-11-30T00:00:00Z"}"#;
        let CloudCredential::InstagramReels {
            login, expires_at, ..
        } = serde_json::from_str(old).unwrap()
        else {
            panic!("an Instagram credential");
        };
        assert_eq!(login, InstagramLogin::InstagramLogin);
        assert!(expires_at.is_some());
    }
}
