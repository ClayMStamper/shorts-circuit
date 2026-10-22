//! Connected destination accounts and their capability records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::AccountId;
use crate::platform::Platform;

/// What the app can do for this account and this application right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublishAccess {
    /// The adapter can upload and publish unattended.
    Automatic,
    /// Only the export package is available.
    ManualOnly { missing: MissingCapability },
}

/// Why automatic publishing is unavailable, with the next action that would fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingCapability {
    pub reason: String,
    pub next_action: NextAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    Reconnect,
    ChangeAccountType,
    RequestApplicationAccess,
    ExportPackage,
}

/// Capability record kept for every connected account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub publish_access: PublishAccess,
    pub native_scheduling: bool,
    pub analytics: bool,
    pub manual_handoff: bool,
    /// Uploads end up private whatever the publication asks for, e.g. while the YouTube API
    /// project hasn't passed Google's compliance audit.
    #[serde(default)]
    pub private_only: bool,
    /// Plain-language limits worth showing next to the account (quotas, visibility).
    #[serde(default)]
    pub notes: Vec<String>,
    pub checked_at: DateTime<Utc>,
}

impl Capabilities {
    /// The record for an account with no API connection: export package only.
    pub fn manual_only(now: DateTime<Utc>) -> Self {
        Self {
            publish_access: PublishAccess::ManualOnly {
                missing: MissingCapability {
                    reason: "No platform API connection is configured for this account.".to_owned(),
                    next_action: NextAction::ExportPackage,
                },
            },
            native_scheduling: false,
            analytics: false,
            manual_handoff: true,
            private_only: false,
            notes: Vec::new(),
            checked_at: now,
        }
    }

    /// The record for an API account whose sign-in is gone (disconnected, expired, revoked):
    /// export packages until the user reconnects.
    pub fn reconnect_required(reason: &str, now: DateTime<Utc>) -> Self {
        Self {
            publish_access: PublishAccess::ManualOnly {
                missing: MissingCapability {
                    reason: reason.to_owned(),
                    next_action: NextAction::Reconnect,
                },
            },
            ..Self::manual_only(now)
        }
    }

    pub fn can_publish_automatically(&self) -> bool {
        matches!(self.publish_access, PublishAccess::Automatic)
    }
}

/// A destination account. Credentials never live here: `credential_ref` names an entry in the
/// OS credential store (desktop) or the backend's encrypted store (cloud).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectedAccount {
    pub id: AccountId,
    pub platform: Platform,
    pub display_name: String,
    pub capabilities: Capabilities,
    pub credential_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ConnectedAccount {
    /// A destination used through manual handoff only.
    pub fn manual(platform: Platform, display_name: &str, now: DateTime<Utc>) -> Self {
        Self {
            id: AccountId::new(),
            platform,
            display_name: display_name.trim().to_owned(),
            capabilities: Capabilities::manual_only(now),
            credential_ref: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_accounts_explain_the_next_action() {
        let now = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        let account = ConnectedAccount::manual(Platform::SnapchatSpotlight, " Studio ", now);
        assert_eq!(account.display_name, "Studio");
        assert!(!account.capabilities.can_publish_automatically());
        assert!(account.capabilities.manual_handoff);
        match account.capabilities.publish_access {
            PublishAccess::ManualOnly { missing } => {
                assert_eq!(missing.next_action, NextAction::ExportPackage)
            }
            PublishAccess::Automatic => panic!("expected manual only"),
        }
    }

    #[test]
    fn disconnected_api_accounts_ask_to_reconnect() {
        let now = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        let c = Capabilities::reconnect_required("Signed out", now);
        assert!(!c.can_publish_automatically());
        assert!(c.manual_handoff);
        assert!(matches!(
            c.publish_access,
            PublishAccess::ManualOnly { missing } if missing.next_action == NextAction::Reconnect
        ));
    }

    #[test]
    fn old_capability_records_still_load() {
        let raw = r#"{"publish_access":{"kind":"automatic"},"native_scheduling":false,
            "analytics":false,"manual_handoff":true,"checked_at":"2026-10-01T00:00:00Z"}"#;
        let c: Capabilities = serde_json::from_str(raw).unwrap();
        assert!(!c.private_only);
        assert!(c.notes.is_empty());
    }
}
