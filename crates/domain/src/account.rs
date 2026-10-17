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
            checked_at: now,
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
}
