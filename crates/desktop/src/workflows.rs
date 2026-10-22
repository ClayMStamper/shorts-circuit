//! Workflows: "source → one destination account" over the source collection's rules and
//! unattended grants (see `shorts_domain::workflow`). This module creates and edits them, says
//! what each one does in plain words, lists their content, and decides when a new file waits
//! briefly for its TikTok caption.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use shorts_domain::workflow::{self, SOURCE_NAME, WorkflowStatus};
use shorts_domain::{
    AccountId, Asset, AssetStatus, Collection, CollectionId, CollectionRule, ConnectedAccount,
    PermissionStatus, Platform, PostingMode, PublicationState, RuleId, RuleOutput, RuleSpec,
    UnattendedGrant, Workflow, WorkflowId, WorkflowSettings,
};
use shorts_store::{PublicationFilter, StoreError};
use tracing::{info, warn};

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::publications::APPROVER;
use crate::views::{
    ActivityView, SourceView, WorkflowDetailView, WorkflowSuggestion, WorkflowView, WorkflowsView,
};

/// The collection created for watch-folder imports when the owner hasn't routed them anywhere.
pub const SOURCE_COLLECTION_NAME: &str = "Source videos";

/// With TikTok new-post detection connected, a new file without a caption of its own waits this
/// long for its TikTok post (so it goes out with the post's caption and hashtags) before a
/// workflow publishes it with a title from the file name.
pub const CAPTION_WAIT_MINUTES: i64 = 120;

/// How far back deferred items are looked at again.
const DEFERRED_LOOKBACK_DAYS: i64 = 3;

/// A workflow as the setup wizard or "Create workflow" submits it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWorkflow {
    pub account_id: AccountId,
    /// Defaults to "TikTok → <platform>".
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "yes")]
    pub auto_publish: bool,
    /// "I own or have the rights to content from this source."
    #[serde(default)]
    pub rights_confirmed: bool,
    /// Defaults to now: nothing that arrived earlier is published.
    #[serde(default)]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub settings: WorkflowSettings,
    #[serde(default)]
    pub caption_template: Option<String>,
    #[serde(default)]
    pub hashtags: Vec<String>,
}

fn yes() -> bool {
    true
}

/// Fields left as `None` are unchanged.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowUpdate {
    pub name: Option<String>,
    /// Active (true) or paused. Reactivating starts from now unless `effective_from` is given.
    pub enabled: Option<bool>,
    pub auto_publish: Option<bool>,
    pub rights_confirmed: Option<bool>,
    pub posting: Option<PostingMode>,
    /// `Some(None)` goes back to the platform default.
    #[serde(default, with = "double_option")]
    pub share_to_feed: Option<Option<bool>>,
    pub caption_template: Option<String>,
    pub hashtags: Option<Vec<String>>,
    pub effective_from: Option<DateTime<Utc>>,
}

mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::deserialize(d)?))
    }
}

fn duplicate(account: &ConnectedAccount) -> AppError {
    AppError::Invalid(
        workflow::WorkflowError::DuplicateDestination(format!(
            "{} · {}",
            account.platform.display_name(),
            account.display_name
        ))
        .to_string(),
    )
}

impl App {
    /// The collection watch-folder imports go to. Creates "Source videos" and routes the watch
    /// folder there when nothing is routed yet.
    pub async fn source_collection(&self) -> AppResult<CollectionId> {
        if let Some(id) = self.preferences().await?.watch_collection
            && self.store.collections().get(id).await?.is_some()
        {
            return Ok(id);
        }
        let existing = self
            .store
            .collections()
            .list()
            .await?
            .into_iter()
            .map(|s| s.collection)
            .find(|c| c.name.eq_ignore_ascii_case(SOURCE_COLLECTION_NAME));
        let id = match existing {
            Some(c) => c.id,
            None => {
                let c = Collection::new(SOURCE_COLLECTION_NAME, self.now())?;
                self.store.collections().insert(&c).await?;
                c.id
            }
        };
        self.update_preferences(crate::PreferencesUpdate {
            watch_collection: Some(Some(id)),
            ..Default::default()
        })
        .await?;
        Ok(id)
    }

    pub async fn create_workflow(&self, new: NewWorkflow) -> AppResult<WorkflowView> {
        let account = self.store.accounts().require(new.account_id).await?;
        let collection = self.source_collection().await?;
        if self
            .store
            .workflows()
            .find(collection, account.id)
            .await?
            .is_some()
        {
            return Err(duplicate(&account));
        }
        let now = self.now();
        let spec = RuleSpec {
            name: new
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| workflow::default_name(account.platform)),
            enabled: true,
            destinations: vec![account.id],
            output: RuleOutput::AwaitingApproval,
            caption_template: new.caption_template.unwrap_or_default(),
            hashtags: new.hashtags,
            include_asset_tags: false,
        };
        let rule = CollectionRule::new(collection, spec, new.effective_from, now)?;
        let w = Workflow::new(
            rule.id,
            collection,
            account.id,
            new.settings,
            new.rights_confirmed,
            now,
        );
        let grant = if new.auto_publish {
            Some(self.grant_for(collection, &account)?)
        } else {
            None
        };
        match self
            .store
            .workflows()
            .create(&rule, &w, grant.as_ref())
            .await
        {
            Ok(()) => {}
            Err(StoreError::DuplicateWorkflow { .. }) => return Err(duplicate(&account)),
            Err(e) => return Err(e.into()),
        }
        info!(workflow = %w.id, account = %account.id, auto = new.auto_publish, "workflow created");
        self.emit(AppEvent::CollectionsChanged);
        self.workflow(w.id).await
    }

    /// An Auto Publish grant, refused for accounts that can't publish automatically.
    fn grant_for(
        &self,
        collection: CollectionId,
        account: &ConnectedAccount,
    ) -> AppResult<UnattendedGrant> {
        if !account.capabilities.can_publish_automatically() {
            return Err(AppError::Invalid(format!(
                "Connect {} first: Auto Publish needs an account the app can post to (export \
                 packages always wait for you).",
                account.display_name
            )));
        }
        Ok(UnattendedGrant {
            collection_id: collection,
            account_id: account.id,
            enabled_at: self.now(),
            enabled_by: APPROVER.to_owned(),
        })
    }

    pub async fn update_workflow(
        &self,
        id: WorkflowId,
        update: WorkflowUpdate,
    ) -> AppResult<WorkflowView> {
        let mut w = self.store.workflows().require(id).await?;
        let mut rule = self.store.collections().require_rule(w.rule_id).await?;
        let account = self.store.accounts().require(w.account_id).await?;
        let now = self.now();

        let mut spec = rule.spec.clone();
        if let Some(name) = update.name {
            spec.name = name;
        }
        if let Some(enabled) = update.enabled {
            spec.enabled = enabled;
        }
        if let Some(template) = update.caption_template {
            spec.caption_template = template;
        }
        if let Some(tags) = update.hashtags {
            spec.hashtags = tags;
        }
        if spec != rule.spec || update.effective_from.is_some() {
            rule.replace(spec, update.effective_from, now)?;
            self.store.collections().update_rule(&rule).await?;
        }

        let mut changed = false;
        if let Some(posting) = update.posting {
            changed |= w.settings.posting != posting;
            w.settings.posting = posting;
        }
        if let Some(feed) = update.share_to_feed {
            changed |= w.settings.share_to_feed != feed;
            w.settings.share_to_feed = feed;
        }
        if let Some(rights) = update.rights_confirmed {
            changed |= w.rights_confirmed_at.is_some() != rights;
            w.confirm_rights(rights, now);
        }
        if changed {
            w.updated_at = now;
            self.store.workflows().update(&w).await?;
        }

        if let Some(auto) = update.auto_publish {
            if auto {
                let grant = self.grant_for(w.collection_id, &account)?;
                self.store.collections().set_grant(&grant).await?;
            } else {
                self.store
                    .collections()
                    .remove_grant(w.collection_id, account.id)
                    .await?;
            }
        }
        self.emit(AppEvent::CollectionsChanged);
        // Turning Auto Publish on approves what this workflow is already holding for review.
        if update.auto_publish == Some(true) {
            for firing in self.store.collections().firings_for_rule(rule.id).await? {
                if let Err(e) = self.unattended_approvals(firing.asset_id).await {
                    warn!(error = %e, "approving waiting items failed");
                }
            }
        }
        self.workflow(id).await
    }

    /// Removes the workflow (its rule and Auto Publish grant). What it already published stays.
    pub async fn delete_workflow(&self, id: WorkflowId) -> AppResult<()> {
        self.store.workflows().delete(id).await?;
        self.emit(AppEvent::CollectionsChanged);
        Ok(())
    }

    pub async fn workflow(&self, id: WorkflowId) -> AppResult<WorkflowView> {
        let w = self.store.workflows().require(id).await?;
        self.workflow_view(&w).await
    }

    /// The Workflows page: every workflow, the source, and suggestions for connected accounts
    /// that don't have one yet.
    pub async fn list_workflows(&self) -> AppResult<WorkflowsView> {
        let mut workflows = Vec::new();
        for w in self.store.workflows().list().await? {
            workflows.push(self.workflow_view(&w).await?);
        }
        let prefs = self.preferences().await?;
        let mut suggestions = Vec::new();
        for account in self.store.accounts().list().await? {
            let taken = workflows
                .iter()
                .any(|w| w.account_id == account.id.to_string());
            if taken || !account.capabilities.can_publish_automatically() {
                continue;
            }
            suggestions.push(WorkflowSuggestion {
                account_id: account.id.to_string(),
                account_name: account.display_name.clone(),
                platform: account.platform,
                platform_name: account.platform.display_name(),
                name: workflow::default_name(account.platform),
            });
        }
        Ok(WorkflowsView {
            workflows,
            suggestions,
            source: SourceView {
                name: SOURCE_NAME,
                watch_folder: prefs.watch_folder,
                watch_checked_at: prefs.watch_checked_at,
                tiktok_connected: self.tiktok_detection_active().await,
            },
        })
    }

    async fn workflow_view(&self, w: &Workflow) -> AppResult<WorkflowView> {
        let rule = self.store.collections().require_rule(w.rule_id).await?;
        let account = self.store.accounts().require(w.account_id).await?;
        let grant = self
            .store
            .collections()
            .grant(w.collection_id, account.id)
            .await?;
        let waiting_for_sign_in = !self
            .store
            .publications()
            .list(&PublicationFilter {
                account_id: Some(account.id),
                states: vec![PublicationState::NeedsAuthentication],
                ..Default::default()
            })
            .await?
            .is_empty();
        let needs_reconnect = workflow::needs_reconnect(&account, waiting_for_sign_in);
        let status = workflow::status(rule.spec.enabled, needs_reconnect);
        let auto_available = account.capabilities.can_publish_automatically();
        let auto_publish = grant.is_some() && auto_available;
        let timezone = self.preferences().await?.timezone;
        let since = rule
            .effective_from
            .with_timezone(&timezone.tz())
            .format("%B %-d, %Y %H:%M %Z")
            .to_string();
        Ok(WorkflowView {
            id: w.id.to_string(),
            name: rule.spec.name.clone(),
            rule_id: rule.id.to_string(),
            account_id: account.id.to_string(),
            account_name: account.display_name.clone(),
            platform: account.platform,
            platform_name: account.platform.display_name(),
            source_name: SOURCE_NAME,
            enabled: rule.spec.enabled,
            auto_publish,
            auto_publish_available: auto_available,
            status,
            status_text: workflow_status_text(status, &account),
            banner: banner(
                &account,
                auto_publish,
                rule.spec.enabled,
                &since,
                w.settings.posting,
            ),
            rights_confirmed: w.rights_confirmed_at.is_some(),
            posting: w.settings.posting,
            share_to_feed: w.settings.share_to_feed,
            caption_template: rule.spec.caption_template.clone(),
            hashtags: rule.spec.hashtags.clone(),
            effective_from: rule.effective_from,
            private_only: account.capabilities.private_only,
            item_count: self.store.collections().fired_count(rule.id).await?,
            created_at: w.created_at,
        })
    }

    /// The workflow and its content: everything it handled, newest first, plus items waiting
    /// for their file or for their TikTok caption.
    pub async fn workflow_detail(&self, id: WorkflowId) -> AppResult<WorkflowDetailView> {
        let w = self.store.workflows().require(id).await?;
        let workflow = self.workflow_view(&w).await?;
        let mut items = self.rule_activity(w.rule_id).await?.items;
        // Members that arrived after the workflow started but haven't fired yet (waiting for
        // their caption).
        let rule = self.store.collections().require_rule(w.rule_id).await?;
        for asset_id in self.store.collections().members(w.collection_id).await? {
            if items.iter().any(|i| i.asset_id == asset_id.to_string()) {
                continue;
            }
            let Some(asset) = self.store.assets().get(asset_id).await? else {
                continue;
            };
            let Some(entered) = self
                .store
                .collections()
                .member_since(w.collection_id, asset_id)
                .await?
            else {
                continue;
            };
            let post = self.store.tiktok_posts().for_asset(asset_id).await?;
            if asset.status == AssetStatus::Ready
                && rule.applies_to(entered, post.as_ref().map(|p| p.posted_at))
                && let Some(until) = self.caption_wait_until(&asset).await?
            {
                let mut item = self.rule_item(asset_id, Vec::new(), false).await?;
                item.waiting_reason = Some(format!(
                    "Waiting for its TikTok post to use that caption (until {}).",
                    until.format("%H:%M UTC")
                ));
                items.push(item);
            }
        }
        items.sort_by_key(|i| std::cmp::Reverse(i.added_at));
        Ok(WorkflowDetailView { workflow, items })
    }

    /// Everything flowing through, newest first, with each destination's status.
    pub async fn activity(&self, limit: u32) -> AppResult<ActivityView> {
        let assets = self
            .store
            .assets()
            .list(&shorts_store::AssetFilter {
                limit: Some(limit),
                ..Default::default()
            })
            .await?;
        let mut items = Vec::with_capacity(assets.len());
        for asset in assets {
            let publications = self
                .store
                .publications()
                .list(&PublicationFilter {
                    asset_id: Some(asset.id),
                    ..Default::default()
                })
                .await?
                .into_iter()
                .map(|p| p.id())
                .collect();
            let waiting = asset.status == AssetStatus::AwaitingMedia;
            let mut item = self.rule_item(asset.id, publications, waiting).await?;
            if !waiting && let Some(until) = self.caption_wait_until(&asset).await? {
                item.waiting_reason = Some(format!(
                    "Waiting for its TikTok post to use that caption (until {}).",
                    until.format("%H:%M UTC")
                ));
            }
            items.push(item);
        }
        Ok(ActivityView { items })
    }

    /// Whether TikTok new-post detection is on and signed in (so captions can come from posts).
    pub(crate) async fn tiktok_detection_active(&self) -> bool {
        crate::tiktok::detection_active(self).await.unwrap_or(false)
    }

    /// When an item without a caption of its own stops waiting for its TikTok post, or `None`
    /// when it doesn't wait (it has a caption or a post, TikTok isn't connected, or the wait
    /// is over).
    pub(crate) async fn caption_wait_until(
        &self,
        asset: &Asset,
    ) -> AppResult<Option<DateTime<Utc>>> {
        if !asset.notes.trim().is_empty()
            || self
                .store
                .tiktok_posts()
                .for_asset(asset.id)
                .await?
                .is_some()
            || !self.tiktok_detection_active().await
        {
            return Ok(None);
        }
        let Some(media) = self
            .store
            .media_files()
            .for_asset(asset.id)
            .await?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let until = media.imported_at + Duration::minutes(CAPTION_WAIT_MINUTES);
        Ok((until > self.now()).then_some(until))
    }

    /// Runs rules again for recent members of workflow collections, so items that waited for
    /// their TikTok caption go out once the wait is over. Cheap and idempotent; called after
    /// each watch-folder rescan and TikTok check.
    pub async fn apply_deferred_rules(&self) {
        let cutoff = self.now() - Duration::days(DEFERRED_LOOKBACK_DAYS);
        let Ok(workflows) = self.store.workflows().list().await else {
            return;
        };
        let mut seen = std::collections::HashSet::new();
        for w in workflows {
            let Ok(members) = self.store.collections().members(w.collection_id).await else {
                continue;
            };
            for asset_id in members {
                if !seen.insert(asset_id) {
                    continue;
                }
                let recent = self
                    .store
                    .collections()
                    .member_since(w.collection_id, asset_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|at| at >= cutoff);
                if recent {
                    self.apply_rules_quietly(asset_id).await;
                }
            }
        }
    }

    /// Records video and audio permission as granted for an item a workflow handles under the
    /// owner's rights confirmation. Decisions already on record (granted or denied) stay.
    pub(crate) async fn grant_rights_for(&self, asset_id: shorts_domain::AssetId, name: &str) {
        let result = async {
            let mut asset = self.store.assets().require(asset_id).await?;
            let now = self.now();
            let note = workflow::rights_note(name);
            let mut changed = false;
            if asset.video_permission.status == PermissionStatus::Unknown {
                asset.record_video_permission(PermissionStatus::Granted, Some(note.clone()), now);
                changed = true;
            }
            if asset.audio_permission.status == PermissionStatus::Unknown {
                asset.record_audio_permission(PermissionStatus::Granted, Some(note), now);
                changed = true;
            }
            if changed {
                asset.updated_at = now;
                self.store.assets().update(&asset).await?;
                self.emit(AppEvent::LibraryChanged);
            }
            Ok::<_, AppError>(())
        }
        .await;
        if let Err(e) = result {
            warn!(asset = %asset_id, error = %e, "could not record permissions");
        }
    }

    pub(crate) async fn workflow_for_rule(&self, rule: RuleId) -> AppResult<Option<Workflow>> {
        Ok(self.store.workflows().for_rule(rule).await?)
    }
}

/// What the status pill's one-click fix says.
fn workflow_status_text(status: WorkflowStatus, account: &ConnectedAccount) -> String {
    match status {
        WorkflowStatus::Active => "Active".into(),
        WorkflowStatus::Inactive => {
            "Paused: new content isn't published until you reactivate it.".into()
        }
        WorkflowStatus::NeedsReconnect => format!(
            "{} needs reconnecting; posts wait and continue once you reconnect.",
            account.display_name
        ),
    }
}

/// "All content from TikTok on or after October 21, 2026 14:00 EDT is published to YouTube
/// Shorts · Channel automatically within ~5 minutes."
fn banner(
    account: &ConnectedAccount,
    auto_publish: bool,
    enabled: bool,
    since: &str,
    posting: PostingMode,
) -> String {
    let destination = format!(
        "{} · {}",
        account.platform.display_name(),
        account.display_name
    );
    if !enabled {
        return format!(
            "Paused. Reactivate it to publish new content from {SOURCE_NAME} to {destination} \
             again (from the moment you reactivate it)."
        );
    }
    let mut text = if auto_publish {
        let when = match posting {
            PostingMode::Asap => "within ~5 minutes of arriving",
            PostingMode::Slots => "in the next free posting slot",
        };
        format!(
            "All content from {SOURCE_NAME} on or after {since} is published to {destination} \
             automatically {when}."
        )
    } else {
        format!(
            "All content from {SOURCE_NAME} on or after {since} is prepared for {destination} \
             and waits for your review."
        )
    };
    if account.platform == Platform::YoutubeShorts && account.capabilities.private_only {
        text.push_str(
            " YouTube keeps these uploads private until you switch the channel to public \
             uploads.",
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banners_say_what_happens_in_plain_words() {
        let mut account = ConnectedAccount::manual(Platform::InstagramReels, "@me", Utc::now());
        let auto = banner(&account, true, true, "October 21, 2026", PostingMode::Asap);
        assert_eq!(
            auto,
            "All content from TikTok on or after October 21, 2026 is published to Instagram \
             Reels · @me automatically within ~5 minutes of arriving."
        );
        assert!(
            banner(&account, false, true, "x", PostingMode::Asap)
                .ends_with("waits for your review.")
        );
        assert!(banner(&account, true, false, "x", PostingMode::Asap).starts_with("Paused."));
        account.platform = Platform::YoutubeShorts;
        account.capabilities.private_only = true;
        assert!(banner(&account, true, true, "x", PostingMode::Slots).contains("private until"));
    }
}
