//! Collections and their automation rules.
//!
//! A collection groups assets. A [`CollectionRule`] says "when an asset enters this collection,
//! create publications for these destinations in the next available slots". Rules only ever
//! create `Draft` or `AwaitingApproval` publications; the one exception is an explicit
//! [`UnattendedGrant`] for a collection *and* account, which lets the rule approve what it
//! created, and only when the account can publish automatically (see [`approval_mode`]).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::account::ConnectedAccount;
use crate::asset::Asset;
use crate::ids::{AccountId, AssetId, CollectionId, PublicationId, RuleId};

pub const MAX_NAME_CHARS: usize = 80;
/// The caption a new rule uses unless the user picks another template.
pub const DEFAULT_CAPTION_TEMPLATE: &str = "{caption}";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CollectionError {
    #[error("give it a name")]
    EmptyName,
    #[error("names can be at most {MAX_NAME_CHARS} characters")]
    NameTooLong,
    #[error("pick at least one destination account")]
    NoDestinations,
    #[error("each destination account can appear only once")]
    DuplicateDestination,
}

fn clean_name(raw: &str) -> Result<String, CollectionError> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        Err(CollectionError::EmptyName)
    } else if name.chars().count() > MAX_NAME_CHARS {
        Err(CollectionError::NameTooLong)
    } else {
        Ok(name)
    }
}

/// A named group of assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Collection {
    pub fn new(name: &str, now: DateTime<Utc>) -> Result<Self, CollectionError> {
        Ok(Self {
            id: CollectionId::new(),
            name: clean_name(name)?,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn rename(&mut self, name: &str, now: DateTime<Utc>) -> Result<(), CollectionError> {
        self.name = clean_name(name)?;
        self.updated_at = now;
        Ok(())
    }
}

/// The state a rule-created publication is left in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOutput {
    /// A draft for the user to finish and submit.
    Draft,
    /// Submitted for review; waits for the user's approval.
    #[default]
    AwaitingApproval,
}

/// What a rule does, independent of its identity. Validated by [`RuleSpec::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSpec {
    pub name: String,
    pub enabled: bool,
    /// Destination accounts, in the order publications are created.
    pub destinations: Vec<AccountId>,
    pub output: RuleOutput,
    /// Caption with `{caption}` (the source caption, e.g. from TikTok, else the title),
    /// `{title}`, `{creator}` and `{notes}` placeholders.
    pub caption_template: String,
    /// Hashtags added to every publication, before the asset's own tags.
    pub hashtags: Vec<String>,
    /// Also use the asset's library tags as hashtags.
    pub include_asset_tags: bool,
}

impl RuleSpec {
    pub fn validate(mut self) -> Result<Self, CollectionError> {
        self.name = clean_name(&self.name)?;
        if self.destinations.is_empty() {
            return Err(CollectionError::NoDestinations);
        }
        let unique: BTreeSet<_> = self.destinations.iter().collect();
        if unique.len() != self.destinations.len() {
            return Err(CollectionError::DuplicateDestination);
        }
        if self.caption_template.trim().is_empty() {
            DEFAULT_CAPTION_TEMPLATE.clone_into(&mut self.caption_template);
        }
        Ok(self)
    }
}

/// "When an asset enters this collection, create publications for these destinations in the
/// next available slots." It fires at most once per asset and destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionRule {
    pub id: RuleId,
    pub collection_id: CollectionId,
    pub spec: RuleSpec,
    /// The rule only acts on assets that entered the collection at or after this instant, and
    /// whose source post (if any) is no older, so turning a rule on never floods destinations
    /// with the back catalog.
    pub effective_from: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CollectionRule {
    /// `effective_from` defaults to now; an earlier instant is an explicit backfill choice.
    pub fn new(
        collection_id: CollectionId,
        spec: RuleSpec,
        effective_from: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Self, CollectionError> {
        Ok(Self {
            id: RuleId::new(),
            collection_id,
            spec: spec.validate()?,
            effective_from: effective_from.unwrap_or(now),
            created_at: now,
            updated_at: now,
        })
    }

    /// Replaces the spec. Turning a disabled rule back on restarts it from now unless an
    /// explicit `effective_from` is given.
    pub fn replace(
        &mut self,
        spec: RuleSpec,
        effective_from: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), CollectionError> {
        let spec = spec.validate()?;
        let reenabled = spec.enabled && !self.spec.enabled;
        self.spec = spec;
        match effective_from {
            Some(at) => self.effective_from = at,
            None if reenabled => self.effective_from = now,
            None => {}
        }
        self.updated_at = now;
        Ok(())
    }

    /// Whether the rule acts on an asset that entered the collection at `entered_at` and whose
    /// source post (if it has one) went up at `source_posted_at`.
    pub fn applies_to(
        &self,
        entered_at: DateTime<Utc>,
        source_posted_at: Option<DateTime<Utc>>,
    ) -> bool {
        self.spec.enabled
            && entered_at >= self.effective_from
            && source_posted_at.is_none_or(|p| p >= self.effective_from)
    }

    /// The caption for `asset`: the template with its placeholders filled in.
    pub fn caption_for(&self, asset: &Asset) -> String {
        let caption = if asset.notes.trim().is_empty() {
            asset.title.as_str()
        } else {
            asset.notes.trim()
        };
        self.spec
            .caption_template
            .replace("{caption}", caption)
            .replace("{title}", &asset.title)
            .replace("{creator}", asset.creator.as_deref().unwrap_or(""))
            .replace("{notes}", asset.notes.trim())
            .trim()
            .to_owned()
    }

    /// The rule's hashtags followed by the asset's tags (when enabled). Normalized later by
    /// the publication revision.
    pub fn hashtags_for(&self, asset: &Asset) -> Vec<String> {
        let mut out = self.spec.hashtags.clone();
        if self.spec.include_asset_tags {
            out.extend(asset.tags.iter().map(|t| t.as_str().to_owned()));
        }
        out
    }
}

/// The owner's explicit opt-in to unattended publishing for one collection and one account.
/// Without it, every rule-created publication waits for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnattendedGrant {
    pub collection_id: CollectionId,
    pub account_id: AccountId,
    pub enabled_at: DateTime<Utc>,
    pub enabled_by: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// The user approves every publication.
    Required,
    /// The rule may approve the publication it created, once, when the media is ready.
    Unattended,
}

/// Unattended publishing needs both an explicit grant for this collection and account and an
/// account that can publish automatically. Anything else requires approval.
pub fn approval_mode(grant: Option<&UnattendedGrant>, account: &ConnectedAccount) -> ApprovalMode {
    match grant {
        Some(g)
            if g.account_id == account.id && account.capabilities.can_publish_automatically() =>
        {
            ApprovalMode::Unattended
        }
        _ => ApprovalMode::Required,
    }
}

/// A rule handled one asset for one destination. The store keeps at most one per
/// `(rule, asset, account)`, which is what makes re-imports and re-adds idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleFiring {
    pub rule_id: RuleId,
    pub asset_id: AssetId,
    pub account_id: AccountId,
    pub publication_id: PublicationId,
    /// The publication was created under an unattended grant.
    pub unattended: bool,
    /// Set once the rule approved it; it never approves the same publication twice.
    pub auto_approved_at: Option<DateTime<Utc>>,
    pub fired_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Capabilities, PublishAccess};
    use crate::asset::Tag;
    use crate::platform::Platform;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    fn spec(destinations: Vec<AccountId>) -> RuleSpec {
        RuleSpec {
            name: "  Cross-post   ".into(),
            enabled: true,
            destinations,
            output: RuleOutput::AwaitingApproval,
            caption_template: String::new(),
            hashtags: vec!["shorts".into()],
            include_asset_tags: true,
        }
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(
            Collection::new(" Best  of ", now()).unwrap().name,
            "Best of"
        );
        assert_eq!(
            Collection::new("   ", now()),
            Err(CollectionError::EmptyName)
        );
        assert_eq!(
            Collection::new(&"x".repeat(81), now()),
            Err(CollectionError::NameTooLong)
        );
    }

    #[test]
    fn rules_need_unique_destinations() {
        let a = AccountId::new();
        let c = CollectionId::new();
        assert_eq!(
            CollectionRule::new(c, spec(vec![]), None, now()),
            Err(CollectionError::NoDestinations)
        );
        assert_eq!(
            CollectionRule::new(c, spec(vec![a, a]), None, now()),
            Err(CollectionError::DuplicateDestination)
        );
        let rule = CollectionRule::new(c, spec(vec![a]), None, now()).unwrap();
        assert_eq!(rule.spec.name, "Cross-post");
        assert_eq!(rule.spec.caption_template, DEFAULT_CAPTION_TEMPLATE);
    }

    #[test]
    fn captions_and_hashtags_come_from_the_asset() {
        let mut asset = Asset::imported("Sourdough", now()).unwrap();
        asset.creator = Some("Bee".into());
        asset.add_tag(Tag::parse("bread").unwrap(), now());
        let mut s = spec(vec![AccountId::new()]);
        s.caption_template = "{title} by {creator} {unknown}".into();
        let rule = CollectionRule::new(CollectionId::new(), s, None, now()).unwrap();
        assert_eq!(rule.caption_for(&asset), "Sourdough by Bee {unknown}");
        assert_eq!(rule.hashtags_for(&asset), vec!["shorts", "bread"]);

        // The default reuses the source caption (hashtags included), else the title.
        let default = CollectionRule::new(
            CollectionId::new(),
            spec(vec![AccountId::new()]),
            None,
            now(),
        )
        .unwrap();
        assert_eq!(default.caption_for(&asset), "Sourdough");
        asset.notes = "Crumb shot #bread
"
        .into();
        assert_eq!(default.caption_for(&asset), "Crumb shot #bread");
    }

    #[test]
    fn rules_start_from_their_effective_time() {
        let later = now() + chrono::Duration::hours(1);
        let mut rule = CollectionRule::new(
            CollectionId::new(),
            spec(vec![AccountId::new()]),
            None,
            now(),
        )
        .unwrap();
        assert!(rule.applies_to(now(), None));
        assert!(!rule.applies_to(now() - chrono::Duration::seconds(1), None));
        // A source post older than the rule doesn't count, even if it entered later.
        assert!(!rule.applies_to(later, Some(now() - chrono::Duration::days(3))));
        assert!(rule.applies_to(later, Some(now())));

        // Turning it off and on restarts it from that moment.
        let mut off = rule.spec.clone();
        off.enabled = false;
        rule.replace(off.clone(), None, now()).unwrap();
        assert!(!rule.applies_to(later, None));
        let mut on = off;
        on.enabled = true;
        rule.replace(on, None, later).unwrap();
        assert_eq!(rule.effective_from, later);
        assert!(!rule.applies_to(now(), None));
    }

    #[test]
    fn approval_is_required_without_a_grant_or_automatic_access() {
        let mut account = ConnectedAccount::manual(Platform::YoutubeShorts, "Main", now());
        let grant = UnattendedGrant {
            collection_id: CollectionId::new(),
            account_id: account.id,
            enabled_at: now(),
            enabled_by: "owner".into(),
        };
        // Manual-only accounts can't publish unattended even with a grant.
        assert_eq!(
            approval_mode(Some(&grant), &account),
            ApprovalMode::Required
        );
        account.capabilities = Capabilities {
            publish_access: PublishAccess::Automatic,
            ..Capabilities::manual_only(now())
        };
        assert_eq!(approval_mode(None, &account), ApprovalMode::Required);
        assert_eq!(
            approval_mode(Some(&grant), &account),
            ApprovalMode::Unattended
        );
        let other = UnattendedGrant {
            account_id: AccountId::new(),
            ..grant
        };
        assert_eq!(
            approval_mode(Some(&other), &account),
            ApprovalMode::Required
        );
    }
}
