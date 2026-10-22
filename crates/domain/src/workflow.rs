//! Workflows: "source → one destination account", the unit the owner works with day to day.
//!
//! A workflow adds no new automation of its own. It is a thin record over a
//! [`CollectionRule`](crate::CollectionRule) with a single destination in the source collection
//! (the one the watch folder routes into): the rule's name, `enabled` flag, caption template,
//! hashtags and `effective_from` are the workflow's; *Auto Publish* is an
//! [`UnattendedGrant`](crate::UnattendedGrant) for that collection and account. What only a
//! workflow has lives here: the owner's rights confirmation and a few settings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::account::{ConnectedAccount, NextAction, PublishAccess};
use crate::ids::{AccountId, CollectionId, RuleId, WorkflowId};
use crate::platform::Platform;
use crate::publication::DestinationOptions;

/// What the source is called in workflow names and banners. Originals arrive through the watch
/// folder, but they are the owner's TikTok videos.
pub const SOURCE_NAME: &str = "TikTok";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkflowError {
    #[error("there is already a workflow from this source to {0}")]
    DuplicateDestination(String),
}

/// When a workflow's publications go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostingMode {
    /// As soon as the video is ready (the default).
    #[default]
    Asap,
    /// In the next free posting slot; with no free slot, as soon as possible.
    Slots,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowSettings {
    #[serde(default)]
    pub posting: PostingMode,
    /// Instagram: also show reels in the profile's main feed. `None` = the platform default
    /// (yes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_to_feed: Option<bool>,
}

impl WorkflowSettings {
    /// The options a new publication for `platform` gets from these settings.
    pub fn destination_options(&self, platform: Platform) -> DestinationOptions {
        match platform {
            Platform::InstagramReels => DestinationOptions {
                share_to_feed: self.share_to_feed,
            },
            _ => DestinationOptions::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: WorkflowId,
    pub rule_id: RuleId,
    pub collection_id: CollectionId,
    pub account_id: AccountId,
    pub settings: WorkflowSettings,
    /// The owner confirmed they own or have the rights to content from this source. Items the
    /// workflow handles then get video and audio permission recorded as granted.
    pub rights_confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Workflow {
    pub fn new(
        rule_id: RuleId,
        collection_id: CollectionId,
        account_id: AccountId,
        settings: WorkflowSettings,
        rights_confirmed: bool,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: WorkflowId::new(),
            rule_id,
            collection_id,
            account_id,
            settings,
            rights_confirmed_at: rights_confirmed.then_some(now),
            created_at: now,
            updated_at: now,
        }
    }

    /// Confirms or withdraws the rights confirmation. Withdrawing only affects items handled
    /// from now on; permissions already recorded stay.
    pub fn confirm_rights(&mut self, confirmed: bool, now: DateTime<Utc>) {
        match (confirmed, self.rights_confirmed_at) {
            (true, None) => self.rights_confirmed_at = Some(now),
            (false, Some(_)) => self.rights_confirmed_at = None,
            _ => return,
        }
        self.updated_at = now;
    }
}

/// The suggested name for a workflow to `platform`, e.g. "TikTok → YouTube Shorts".
pub fn default_name(platform: Platform) -> String {
    format!("{SOURCE_NAME} → {}", platform.display_name())
}

/// Who a workflow's approvals are recorded as.
pub fn approver(name: &str) -> String {
    format!("workflow:{name}")
}

/// The permission note recorded for items a workflow handles under a rights confirmation.
pub fn rights_note(name: &str) -> String {
    format!("Rights confirmed in workflow \"{name}\"")
}

/// Active / Inactive / Needs reconnect, as the Workflows page shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Active,
    /// Paused by the owner; nothing new is created until it is reactivated.
    Inactive,
    /// The destination's sign-in is gone; posts wait until it is reconnected.
    NeedsReconnect,
}

/// Whether `account` needs a reconnect before it can publish again: its capability record says
/// so, or a publication for it is waiting at `NeedsAuthentication`.
pub fn needs_reconnect(account: &ConnectedAccount, waiting_for_sign_in: bool) -> bool {
    waiting_for_sign_in
        || matches!(
            &account.capabilities.publish_access,
            PublishAccess::ManualOnly { missing } if missing.next_action == NextAction::Reconnect
        )
}

pub fn status(enabled: bool, needs_reconnect: bool) -> WorkflowStatus {
    if needs_reconnect {
        WorkflowStatus::NeedsReconnect
    } else if enabled {
        WorkflowStatus::Active
    } else {
        WorkflowStatus::Inactive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Capabilities;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    #[test]
    fn names_and_approvers() {
        assert_eq!(
            default_name(Platform::YoutubeShorts),
            "TikTok → YouTube Shorts"
        );
        assert_eq!(approver("TikTok → LinkedIn"), "workflow:TikTok → LinkedIn");
    }

    #[test]
    fn rights_confirmation_is_timestamped_once() {
        let mut w = Workflow::new(
            RuleId::new(),
            CollectionId::new(),
            AccountId::new(),
            WorkflowSettings::default(),
            false,
            now(),
        );
        assert_eq!(w.rights_confirmed_at, None);
        let later = now() + chrono::Duration::hours(1);
        w.confirm_rights(true, later);
        assert_eq!(w.rights_confirmed_at, Some(later));
        w.confirm_rights(true, later + chrono::Duration::hours(1));
        assert_eq!(w.rights_confirmed_at, Some(later));
        w.confirm_rights(false, later);
        assert_eq!(w.rights_confirmed_at, None);
    }

    #[test]
    fn status_prefers_reconnect_then_paused() {
        let mut account = ConnectedAccount::manual(Platform::YoutubeShorts, "Main", now());
        assert!(!needs_reconnect(&account, false));
        assert!(needs_reconnect(&account, true));
        account.capabilities = Capabilities::reconnect_required("expired", now());
        assert!(needs_reconnect(&account, false));
        assert_eq!(status(true, true), WorkflowStatus::NeedsReconnect);
        assert_eq!(status(false, false), WorkflowStatus::Inactive);
        assert_eq!(status(true, false), WorkflowStatus::Active);
    }

    #[test]
    fn only_instagram_gets_share_to_feed() {
        let s = WorkflowSettings {
            posting: PostingMode::Asap,
            share_to_feed: Some(false),
        };
        assert_eq!(
            s.destination_options(Platform::InstagramReels)
                .share_to_feed,
            Some(false)
        );
        assert!(s.destination_options(Platform::Linkedin).is_default());
    }
}
