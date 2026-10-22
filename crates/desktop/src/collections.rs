//! Collections and rules: assets entering a collection get publications for the rule's
//! destinations in the next free posting slots. Approval stays required unless the owner
//! enabled unattended publishing for that collection and account.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use shorts_domain::copy;
use shorts_domain::{
    AccountId, ApprovalMode, AssetId, AssetStatus, Collection, CollectionId, CollectionRule,
    Destination, PostingMode, Publication, PublicationEvent, PublicationId, PublicationState,
    RevisionContent, RuleFiring, RuleId, RuleOutput, RuleSpec, Schedule, UnattendedGrant,
    approval_mode,
};
use tracing::{info, warn};

use crate::ScheduleInput;
use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::publications::APPROVER;
use crate::views::{
    CollectionRef, CollectionView, RuleActivityView, RuleDestinationView, RuleItemView, RuleView,
    UnattendedView,
};

/// A rule as the UI submits it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleInput {
    pub name: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    pub destinations: Vec<AccountId>,
    #[serde(default)]
    pub output: RuleOutput,
    #[serde(default)]
    pub caption_template: String,
    #[serde(default)]
    pub hashtags: Vec<String>,
    #[serde(default)]
    pub include_asset_tags: bool,
    /// Act only on assets entering from this instant (and with no older source post). Omitted:
    /// from now, or unchanged when editing.
    #[serde(default)]
    pub effective_from: Option<DateTime<Utc>>,
}

fn enabled_default() -> bool {
    true
}

impl From<RuleInput> for RuleSpec {
    fn from(input: RuleInput) -> Self {
        Self {
            name: input.name,
            enabled: input.enabled,
            destinations: input.destinations,
            output: input.output,
            caption_template: input.caption_template,
            hashtags: input.hashtags,
            include_asset_tags: input.include_asset_tags,
        }
    }
}

impl App {
    pub async fn list_collections(&self) -> AppResult<Vec<CollectionView>> {
        let mut out = Vec::new();
        for summary in self.store.collections().list().await? {
            out.push(
                self.collection_view(&summary.collection, summary.member_count)
                    .await?,
            );
        }
        Ok(out)
    }

    pub async fn collection(&self, id: CollectionId) -> AppResult<CollectionView> {
        let c = self.store.collections().require(id).await?;
        let members = self.store.collections().members(id).await?.len();
        self.collection_view(&c, u32::try_from(members).unwrap_or(u32::MAX))
            .await
    }

    pub async fn create_collection(&self, name: &str) -> AppResult<CollectionView> {
        let c = Collection::new(name, self.now())?;
        self.store.collections().insert(&c).await?;
        self.emit(AppEvent::CollectionsChanged);
        self.collection_view(&c, 0).await
    }

    pub async fn rename_collection(
        &self,
        id: CollectionId,
        name: &str,
    ) -> AppResult<CollectionView> {
        let mut c = self.store.collections().require(id).await?;
        c.rename(name, self.now())?;
        self.store.collections().update(&c).await?;
        self.emit(AppEvent::CollectionsChanged);
        self.collection(id).await
    }

    /// Deletes the collection, its rules and grants. Assets and publications stay; if the
    /// watch folder was routed here, routing is turned off.
    pub async fn delete_collection(&self, id: CollectionId) -> AppResult<()> {
        self.store.collections().delete(id).await?;
        if self.preferences().await?.watch_collection == Some(id) {
            self.clear_watch_collection().await?;
        }
        self.emit(AppEvent::CollectionsChanged);
        Ok(())
    }

    /// Adds an asset and runs the collection's rules for it. Returns the publications created.
    pub async fn add_to_collection(
        &self,
        collection: CollectionId,
        asset: AssetId,
    ) -> AppResult<Vec<PublicationId>> {
        self.store.collections().require(collection).await?;
        self.store.assets().require(asset).await?;
        self.store
            .collections()
            .add_member(collection, asset, self.now())
            .await?;
        self.emit(AppEvent::CollectionsChanged);
        self.emit(AppEvent::LibraryChanged);
        self.apply_rules(asset).await
    }

    /// Removes an asset. Publications the rules already created are left alone.
    pub async fn remove_from_collection(
        &self,
        collection: CollectionId,
        asset: AssetId,
    ) -> AppResult<()> {
        self.store
            .collections()
            .remove_member(collection, asset)
            .await?;
        self.emit(AppEvent::CollectionsChanged);
        self.emit(AppEvent::LibraryChanged);
        Ok(())
    }

    pub async fn asset_collections(&self, asset: AssetId) -> AppResult<Vec<CollectionRef>> {
        Ok(self
            .store
            .collections()
            .of_asset(asset)
            .await?
            .into_iter()
            .map(|c| CollectionRef {
                id: c.id.to_string(),
                name: c.name,
            })
            .collect())
    }

    /// New rules apply to assets that enter the collection from now on.
    pub async fn create_rule(
        &self,
        collection: CollectionId,
        input: RuleInput,
    ) -> AppResult<CollectionView> {
        self.store.collections().require(collection).await?;
        let from = input.effective_from;
        let rule = CollectionRule::new(collection, input.into(), from, self.now())?;
        self.check_destinations(&rule.spec).await?;
        self.store.collections().insert_rule(&rule).await?;
        self.emit(AppEvent::CollectionsChanged);
        self.collection(collection).await
    }

    pub async fn update_rule(&self, id: RuleId, input: RuleInput) -> AppResult<CollectionView> {
        let mut rule = self.store.collections().require_rule(id).await?;
        let from = input.effective_from;
        rule.replace(input.into(), from, self.now())?;
        self.check_destinations(&rule.spec).await?;
        self.store.collections().update_rule(&rule).await?;
        self.emit(AppEvent::CollectionsChanged);
        self.collection(rule.collection_id).await
    }

    pub async fn delete_rule(&self, id: RuleId) -> AppResult<CollectionView> {
        let rule = self.store.collections().require_rule(id).await?;
        // A workflow's rule takes the workflow (and its Auto Publish grant) with it.
        if let Some(w) = self.workflow_for_rule(id).await? {
            self.store.workflows().delete(w.id).await?;
            self.emit(AppEvent::CollectionsChanged);
            return self.collection(rule.collection_id).await;
        }
        self.store.collections().delete_rule(id).await?;
        self.emit(AppEvent::CollectionsChanged);
        self.collection(rule.collection_id).await
    }

    async fn check_destinations(&self, spec: &RuleSpec) -> AppResult<()> {
        for id in &spec.destinations {
            self.store.accounts().require(*id).await?;
        }
        Ok(())
    }

    /// Turns unattended publishing on or off for one collection and account. It can only be
    /// turned on for accounts that publish automatically; export-package accounts always wait
    /// for approval.
    pub async fn set_unattended(
        &self,
        collection: CollectionId,
        account: AccountId,
        enabled: bool,
    ) -> AppResult<CollectionView> {
        self.store.collections().require(collection).await?;
        let acc = self.store.accounts().require(account).await?;
        if enabled {
            if !acc.capabilities.can_publish_automatically() {
                return Err(AppError::Invalid(format!(
                    "{} publishes through export packages, so every publication needs your \
                     approval; unattended publishing needs an account that can publish \
                     automatically",
                    acc.display_name
                )));
            }
            self.store
                .collections()
                .set_grant(&UnattendedGrant {
                    collection_id: collection,
                    account_id: account,
                    enabled_at: self.now(),
                    enabled_by: APPROVER.to_owned(),
                })
                .await?;
            info!(%collection, %account, "unattended publishing enabled");
        } else {
            self.store
                .collections()
                .remove_grant(collection, account)
                .await?;
        }
        self.emit(AppEvent::CollectionsChanged);
        self.collection(collection).await
    }

    /// Runs every enabled rule of every collection the asset is in. Assets still waiting for
    /// media are skipped and picked up when their media arrives. Each rule fires at most once
    /// per asset and destination, so calling this again is harmless.
    pub async fn apply_rules(&self, asset_id: AssetId) -> AppResult<Vec<PublicationId>> {
        let asset = self.store.assets().require(asset_id).await?;
        if asset.status != AssetStatus::Ready {
            return Ok(Vec::new());
        }
        let Some(variant) = self
            .store
            .variants()
            .for_asset(asset_id)
            .await?
            .into_iter()
            .next()
        else {
            return Ok(Vec::new());
        };
        let prefs = self.preferences().await?;
        let source_posted_at = self
            .store
            .tiktok_posts()
            .for_asset(asset_id)
            .await?
            .map(|p| p.posted_at);
        let mut created = Vec::new();
        let mut unattended = false;
        for collection in self.store.collections().of_asset(asset_id).await? {
            let Some(entered_at) = self
                .store
                .collections()
                .member_since(collection.id, asset_id)
                .await?
            else {
                continue;
            };
            for rule in self.store.collections().rules(collection.id).await? {
                // Disabled, or the asset predates the rule: never flood with the back catalog.
                if !rule.applies_to(entered_at, source_posted_at) {
                    continue;
                }
                let workflow = self.workflow_for_rule(rule.id).await?;
                if workflow.is_some() {
                    // A new file without a caption waits briefly for its TikTok post;
                    // `apply_deferred_rules` comes back for it.
                    let current = self.store.assets().require(asset_id).await?;
                    if self.caption_wait_until(&current).await?.is_some() {
                        continue;
                    }
                    if workflow
                        .as_ref()
                        .is_some_and(|w| w.rights_confirmed_at.is_some())
                    {
                        self.grant_rights_for(asset_id, &rule.spec.name).await;
                    }
                }
                // Re-read: the caption may have come from a match, permissions from a workflow.
                let asset = self.store.assets().require(asset_id).await?;
                for account_id in &rule.spec.destinations {
                    if self
                        .store
                        .collections()
                        .firing_exists(rule.id, asset_id, *account_id)
                        .await?
                    {
                        continue;
                    }
                    let Some(account) = self.store.accounts().get(*account_id).await? else {
                        warn!(rule = %rule.id, account = %account_id, "rule destination is gone");
                        continue;
                    };
                    let grant = self
                        .store
                        .collections()
                        .grant(collection.id, account.id)
                        .await?;
                    let mode = approval_mode(grant.as_ref(), &account);
                    let destination = Destination {
                        account_id: account.id,
                        platform: account.platform,
                    };
                    let caption = rule.caption_for(&asset);
                    let hashtags = rule.hashtags_for(&asset);
                    let content = match &workflow {
                        // Workflows take the source's caption and hashtags and fit them to the
                        // destination, so review has nothing to flag.
                        Some(w) => {
                            let fitted = copy::fit(
                                account.platform,
                                &copy::from_source(&caption, &hashtags),
                            );
                            let mut content = RevisionContent::new(
                                variant.id,
                                variant.revision(),
                                &fitted.caption,
                                &fitted.hashtags,
                                destination,
                            )?;
                            content.options = w.settings.destination_options(account.platform);
                            content
                        }
                        None => RevisionContent::new(
                            variant.id,
                            variant.revision(),
                            &caption,
                            &hashtags,
                            destination,
                        )?,
                    };
                    let slot = self.next_slot(account.id).await?;
                    let at = match &workflow {
                        // As soon as possible, or the next free slot (now when none is free).
                        Some(w) => match w.settings.posting {
                            PostingMode::Asap => Some(self.now()),
                            PostingMode::Slots => slot.or_else(|| Some(self.now())),
                        },
                        // No free slot (e.g. none configured) leaves it unscheduled; approval
                        // then waits for the user to pick a time.
                        None => slot,
                    };
                    let schedule = at.map(|at| Schedule::new(at, prefs.timezone));
                    let publication = Publication::draft(asset_id, content, schedule, self.now());
                    let firing = RuleFiring {
                        rule_id: rule.id,
                        asset_id,
                        account_id: account.id,
                        publication_id: publication.id(),
                        unattended: mode == ApprovalMode::Unattended,
                        auto_approved_at: None,
                        fired_at: self.now(),
                    };
                    if !self.store.record_rule_firing(&firing, &publication).await? {
                        continue;
                    }
                    info!(rule = %rule.id, asset = %asset_id, publication = %publication.id(), "rule created a publication");
                    if rule.spec.output == RuleOutput::AwaitingApproval
                        || mode == ApprovalMode::Unattended
                    {
                        self.transition(publication.id(), |_| PublicationEvent::SubmitForReview)
                            .await?;
                    }
                    self.emit(AppEvent::PublicationChanged {
                        id: publication.id().to_string(),
                    });
                    unattended |= mode == ApprovalMode::Unattended;
                    created.push(publication.id());
                }
            }
        }
        if !created.is_empty() {
            // Render right away so review (or the unattended approval below, which needs
            // rendered media) doesn't wait for anyone to press Render.
            self.render_variant(variant.id).await?;
        }
        if unattended {
            self.unattended_approvals(asset_id).await?;
        }
        Ok(created)
    }

    /// [`App::apply_rules`] for callers that must not fail because a rule did (imports).
    pub(crate) async fn apply_rules_quietly(&self, asset_id: AssetId) {
        if let Err(e) = self.apply_rules(asset_id).await {
            warn!(asset = %asset_id, error = %e, "collection rules failed");
            self.emit(AppEvent::RulesFailed {
                asset_id: asset_id.to_string(),
                message: e.to_string(),
            });
        }
    }

    /// Approves publications that a rule created under an unattended grant, once each, when
    /// the grant still exists, the account still publishes automatically, the media is
    /// rendered and the review shows no issues. Anything else is left for the user.
    pub(crate) async fn unattended_approvals(&self, asset_id: AssetId) -> AppResult<()> {
        for firing in self.store.collections().firings_for_asset(asset_id).await? {
            if firing.auto_approved_at.is_some() {
                continue;
            }
            // A workflow follows its current Auto Publish switch (turning it on approves what
            // it was holding); a plain rule only what it created under a grant.
            let workflow = self.workflow_for_rule(firing.rule_id).await?;
            if !firing.unattended && workflow.is_none() {
                continue;
            }
            let Some(p) = self.store.publications().get(firing.publication_id).await? else {
                continue;
            };
            // Only the revision the rule created: an edit by hand goes back to the owner.
            if p.state() != PublicationState::AwaitingApproval
                || p.content().destination.account_id != firing.account_id
                || p.revision().number != 1
            {
                continue;
            }
            let Some(rule) = self.store.collections().rule(firing.rule_id).await? else {
                continue;
            };
            let account = self.store.accounts().require(firing.account_id).await?;
            let grant = self
                .store
                .collections()
                .grant(rule.collection_id, account.id)
                .await?;
            if approval_mode(grant.as_ref(), &account) != ApprovalMode::Unattended {
                continue;
            }
            // An "as soon as possible" time that passed while rendering moves to now, so the
            // overdue policy doesn't expire it.
            let asap = workflow
                .as_ref()
                .is_some_and(|w| w.settings.posting == PostingMode::Asap);
            if asap && p.schedule().is_none_or(|s| s.at < self.now()) {
                self.schedule(p.id(), ScheduleInput::Now).await?;
            }
            let p = self.store.publications().require(p.id()).await?;
            let checks = self.checks(&p).await?;
            if !checks.can_approve || !checks.issues.is_empty() || checks.video_denied {
                continue;
            }
            if !self
                .store
                .collections()
                .mark_auto_approved(p.id(), self.now())
                .await?
            {
                continue;
            }
            let by = match &workflow {
                Some(_) => shorts_domain::workflow::approver(&rule.spec.name),
                None => format!("rule:{}", rule.spec.name),
            };
            match self.approve_as(p.id(), p.revision().number, &by).await {
                Ok(_) => info!(publication = %p.id(), rule = %rule.id, "approved unattended"),
                Err(e) => warn!(publication = %p.id(), error = %e, "unattended approval failed"),
            }
        }
        Ok(())
    }

    async fn rule_view(
        &self,
        c: &Collection,
        rule: &CollectionRule,
        accounts: &[shorts_domain::ConnectedAccount],
        grants: &[UnattendedGrant],
        timezone: shorts_domain::TimeZoneName,
    ) -> AppResult<RuleView> {
        let destinations: Vec<RuleDestinationView> = rule
            .spec
            .destinations
            .iter()
            .map(|id| {
                let account = accounts.iter().find(|a| a.id == *id);
                RuleDestinationView {
                    account_id: id.to_string(),
                    account_name: account.map(|a| a.display_name.clone()).unwrap_or_default(),
                    platform_name: account.map(|a| a.platform.display_name()).unwrap_or(""),
                    unattended: account.is_some_and(|a| {
                        approval_mode(grants.iter().find(|g| g.account_id == a.id), a)
                            == ApprovalMode::Unattended
                    }),
                }
            })
            .collect();
        Ok(RuleView {
            id: rule.id.to_string(),
            name: rule.spec.name.clone(),
            enabled: rule.spec.enabled,
            summary: rule_summary(c, rule, &destinations, timezone),
            destinations,
            output: rule.spec.output,
            caption_template: rule.spec.caption_template.clone(),
            hashtags: rule.spec.hashtags.clone(),
            include_asset_tags: rule.spec.include_asset_tags,
            effective_from: rule.effective_from,
            fired_count: self.store.collections().fired_count(rule.id).await?,
        })
    }

    /// One rule's work, like a workflow's content list: every asset it created publications
    /// for (with each destination's state and next actions), plus members waiting for media.
    pub async fn rule_activity(&self, id: RuleId) -> AppResult<RuleActivityView> {
        let rule = self.store.collections().require_rule(id).await?;
        let c = self.store.collections().require(rule.collection_id).await?;
        let accounts = self.store.accounts().list().await?;
        let grants = self.store.collections().grants(c.id).await?;
        let timezone = self.preferences().await?.timezone;
        let view = self
            .rule_view(&c, &rule, &accounts, &grants, timezone)
            .await?;

        let mut order: Vec<AssetId> = Vec::new();
        let mut by_asset: std::collections::HashMap<AssetId, Vec<PublicationId>> =
            std::collections::HashMap::new();
        for firing in self.store.collections().firings_for_rule(id).await? {
            if !by_asset.contains_key(&firing.asset_id) {
                order.push(firing.asset_id);
            }
            by_asset
                .entry(firing.asset_id)
                .or_default()
                .push(firing.publication_id);
        }
        let mut items = Vec::new();
        // Members that will get publications once their original file arrives.
        for asset_id in self.store.collections().members(c.id).await? {
            if by_asset.contains_key(&asset_id) {
                continue;
            }
            let Some(asset) = self.store.assets().get(asset_id).await? else {
                continue;
            };
            let entered = self
                .store
                .collections()
                .member_since(c.id, asset_id)
                .await?;
            let post = self.store.tiktok_posts().for_asset(asset_id).await?;
            let eligible =
                entered.is_some_and(|at| rule.applies_to(at, post.as_ref().map(|p| p.posted_at)));
            if asset.status == AssetStatus::AwaitingMedia && eligible {
                items.push(self.rule_item(asset_id, Vec::new(), true).await?);
            }
        }
        for asset_id in order {
            let ids = by_asset.remove(&asset_id).unwrap_or_default();
            items.push(self.rule_item(asset_id, ids, false).await?);
        }
        Ok(RuleActivityView { rule: view, items })
    }

    pub(crate) async fn rule_item(
        &self,
        asset_id: AssetId,
        publications: Vec<PublicationId>,
        waiting_for_media: bool,
    ) -> AppResult<RuleItemView> {
        let asset = self.store.assets().get(asset_id).await?;
        let post = self.store.tiktok_posts().for_asset(asset_id).await?;
        let duration_ms = self
            .store
            .media_files()
            .for_asset(asset_id)
            .await?
            .first()
            .map(|m| m.properties.duration_ms)
            .or_else(|| post.as_ref().and_then(|p| p.duration_ms));
        let mut views = Vec::new();
        for id in publications {
            if let Some(p) = self.store.publications().get(id).await? {
                views.push(self.publication_view(&p).await?);
            }
        }
        let (expecting_original_until, download_note) = post
            .as_ref()
            .map(|p| {
                (
                    self.expected_until(&p.video_id),
                    self.download_note(&p.video_id),
                )
            })
            .unwrap_or_default();
        let source_url = post
            .as_ref()
            .and_then(|p| p.share_url.clone())
            .or_else(|| asset.as_ref().and_then(|a| a.source_url.clone()))
            .filter(|u| u.starts_with("https://"));
        Ok(RuleItemView {
            asset_id: asset_id.to_string(),
            added_at: asset.as_ref().map_or_else(|| self.now(), |a| a.created_at),
            waiting_reason: None,
            title: asset.as_ref().map(|a| a.title.clone()).unwrap_or_default(),
            caption: asset.map(|a| a.notes).unwrap_or_default(),
            duration_ms,
            source_posted_at: post.map(|p| p.posted_at),
            source_url,
            expecting_original_until,
            download_note,
            waiting_for_media,
            publications: views,
        })
    }

    async fn collection_view(
        &self,
        c: &Collection,
        member_count: u32,
    ) -> AppResult<CollectionView> {
        let accounts = self.store.accounts().list().await?;
        let grants = self.store.collections().grants(c.id).await?;
        let timezone = self.preferences().await?.timezone;
        let mut rules = Vec::new();
        for rule in self.store.collections().rules(c.id).await? {
            rules.push(
                self.rule_view(c, &rule, &accounts, &grants, timezone)
                    .await?,
            );
        }
        let unattended = accounts
            .iter()
            .map(|a| {
                let grant = grants.iter().find(|g| g.account_id == a.id);
                let available = a.capabilities.can_publish_automatically();
                UnattendedView {
                    account_id: a.id.to_string(),
                    account_name: a.display_name.clone(),
                    platform_name: a.platform.display_name(),
                    enabled: grant.is_some() && available,
                    available,
                    unavailable_reason: (!available).then(|| {
                        "Publishes through export packages, so approval is always required."
                            .to_owned()
                    }),
                    enabled_at: grant.map(|g| g.enabled_at),
                }
            })
            .collect();
        let watch_target = self.preferences().await?.watch_collection == Some(c.id);
        Ok(CollectionView {
            id: c.id.to_string(),
            name: c.name.clone(),
            member_count,
            rules,
            unattended,
            watch_folder_target: watch_target,
        })
    }
}

/// The rule in plain words, e.g. "Videos that enter "Phone" on or after Fri 2026-10-30 08:00 EDT
/// get publications for YouTube Shorts · Channel in their next free posting slots, waiting for
/// your approval."
fn rule_summary(
    c: &Collection,
    rule: &CollectionRule,
    destinations: &[RuleDestinationView],
    timezone: shorts_domain::TimeZoneName,
) -> String {
    if !rule.spec.enabled {
        return "Off: nothing new is created until you turn it on again.".to_owned();
    }
    let since = rule
        .effective_from
        .with_timezone(&timezone.tz())
        .format("%a %Y-%m-%d %H:%M %Z");
    let names: Vec<String> = destinations
        .iter()
        .map(|d| format!("{} · {}", d.platform_name, d.account_name))
        .collect();
    let mode = match rule.spec.output {
        RuleOutput::Draft => "as drafts for you to finish",
        RuleOutput::AwaitingApproval => "waiting for your approval",
    };
    let unattended: Vec<&str> = destinations
        .iter()
        .filter(|d| d.unattended)
        .map(|d| d.account_name.as_str())
        .collect();
    let mut text = format!(
        "Videos that enter \"{}\" on or after {since} (TikTok posts: posted on or after then) get \
         publications for {} in their next free posting slots, {mode}.",
        c.name,
        names.join(", ")
    );
    if !unattended.is_empty() {
        text.push_str(&format!(
            " Unattended publishing is on for {}: those are approved and posted without asking.",
            unattended.join(", ")
        ));
    }
    text
}
