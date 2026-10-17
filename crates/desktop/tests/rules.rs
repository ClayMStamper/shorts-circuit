//! Collections and collection rules: firing, slot allocation, idempotence, and approval.

mod support;

use std::sync::Arc;

use chrono::{NaiveTime, Weekday};
use shorts_adapters::AdapterRegistry;
use shorts_adapters::fake::FakeAdapter;
use shorts_desktop::{AssetUpdate, ImportOutcome, PermissionInput, PreferencesUpdate, RuleInput};
use shorts_domain::{
    AccountId, AssetId, Capabilities, CollectionId, ConnectedAccount, JobKind, PermissionStatus,
    Platform, PublicationId, PublicationState, PublishAccess, RuleOutput,
};
use shorts_scheduler::PostingSlot;
use shorts_store::{JobFilter, PublicationFilter};
use support::{Harness, harness, harness_with, t0};

/// Mon/Wed 18:00 New York. With the clock at Friday 2026-10-30 12:00Z the slots are
/// Mon 2 Nov 23:00Z, Wed 4 Nov 23:00Z, Mon 9 Nov 23:00Z, ...
async fn posting_slots(h: &Harness) {
    h.app
        .update_preferences(PreferencesUpdate {
            timezone: Some("America/New_York".into()),
            posting_slots: Some(vec![PostingSlot {
                weekdays: vec![Weekday::Mon, Weekday::Wed],
                time: NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            }]),
            ..Default::default()
        })
        .await
        .unwrap();
}

async fn account(h: &Harness, platform: Platform, name: &str) -> AccountId {
    h.app
        .add_manual_account(platform, name)
        .await
        .unwrap()
        .id
        .parse()
        .unwrap()
}

async fn automatic_account(h: &Harness, platform: Platform) -> AccountId {
    let mut account = ConnectedAccount::manual(platform, "API account", t0());
    account.capabilities = Capabilities {
        publish_access: PublishAccess::Automatic,
        ..Capabilities::manual_only(t0())
    };
    h.app.store().accounts().insert(&account).await.unwrap();
    account.id
}

async fn collection(h: &Harness, name: &str) -> CollectionId {
    h.app
        .create_collection(name)
        .await
        .unwrap()
        .id
        .parse()
        .unwrap()
}

fn rule(destinations: Vec<AccountId>) -> RuleInput {
    RuleInput {
        name: "Cross-post".into(),
        enabled: true,
        destinations,
        output: RuleOutput::AwaitingApproval,
        caption_template: "{title} 🍞".into(),
        hashtags: vec!["shorts".into()],
        include_asset_tags: true,
        effective_from: None,
    }
}

async fn imported(h: &Harness, name: &str) -> AssetId {
    let path = h.video(name, format!("bytes of {name}").as_bytes());
    h.app.import_file(&path, None).await.unwrap().asset_id()
}

async fn publications(h: &Harness, asset: AssetId) -> Vec<shorts_domain::Publication> {
    h.app
        .store()
        .publications()
        .list(&PublicationFilter {
            asset_id: Some(asset),
            ..Default::default()
        })
        .await
        .unwrap()
}

async fn publish_jobs(h: &Harness) -> usize {
    h.app
        .store()
        .jobs()
        .list(&JobFilter::default())
        .await
        .unwrap()
        .iter()
        .filter(|j| j.kind() == JobKind::PublishPublication)
        .count()
}

async fn grant_permissions(h: &Harness, asset: AssetId) {
    let granted = || {
        Some(PermissionInput {
            status: PermissionStatus::Granted,
            note: None,
        })
    };
    h.app
        .update_asset(
            asset,
            AssetUpdate {
                video_permission: granted(),
                audio_permission: granted(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn entering_a_collection_creates_publications_that_wait_for_approval() {
    let h = harness().await;
    posting_slots(&h).await;
    let yt = account(&h, Platform::YoutubeShorts, "Channel").await;
    let ig = account(&h, Platform::InstagramReels, "Reels").await;
    let c = collection(&h, "Cross-post").await;
    h.app.create_rule(c, rule(vec![yt, ig])).await.unwrap();

    let asset = imported(&h, "sourdough.mp4").await;
    h.app
        .update_asset(
            asset,
            AssetUpdate {
                add_tags: vec!["bread".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let created = h.app.add_to_collection(c, asset).await.unwrap();
    assert_eq!(created.len(), 2);

    let pubs = publications(&h, asset).await;
    assert_eq!(pubs.len(), 2);
    for p in &pubs {
        assert_eq!(p.state(), PublicationState::AwaitingApproval);
        assert!(!p.has_valid_approval());
        assert_eq!(p.content().caption, "sourdough 🍞");
        assert_eq!(p.content().hashtags, vec!["shorts", "bread"]);
        // Each account's first free slot: Monday 2 November 18:00 EST.
        assert_eq!(
            p.schedule().unwrap().at.to_rfc3339(),
            "2026-11-02T23:00:00+00:00"
        );
        assert_eq!(p.schedule().unwrap().timezone.name(), "America/New_York");
    }
    // Nothing goes out without approval.
    assert_eq!(publish_jobs(&h).await, 0);
    let view = h.app.collection(c).await.unwrap();
    assert_eq!(view.member_count, 1);
    assert_eq!(view.rules[0].fired_count, 2);
    assert!(view.rules[0].destinations.iter().all(|d| !d.unattended));
    let detail = h.app.asset_detail(asset).await.unwrap();
    assert_eq!(detail.collections[0].name, "Cross-post");
}

#[tokio::test]
async fn later_assets_take_the_next_free_slots() {
    let h = harness().await;
    posting_slots(&h).await;
    let yt = account(&h, Platform::YoutubeShorts, "Channel").await;
    let c = collection(&h, "Queue").await;
    h.app.create_rule(c, rule(vec![yt])).await.unwrap();

    let mut slots = Vec::new();
    for name in ["one.mp4", "two.mp4", "three.mp4"] {
        let asset = imported(&h, name).await;
        h.app.add_to_collection(c, asset).await.unwrap();
        let p = publications(&h, asset).await.remove(0);
        slots.push(p.schedule().unwrap().at.to_rfc3339());
    }
    assert_eq!(
        slots,
        vec![
            "2026-11-02T23:00:00+00:00",
            "2026-11-04T23:00:00+00:00",
            "2026-11-09T23:00:00+00:00",
        ]
    );
}

#[tokio::test]
async fn without_posting_slots_publications_are_left_unscheduled() {
    let h = harness().await;
    let yt = account(&h, Platform::YoutubeShorts, "Channel").await;
    let c = collection(&h, "No slots").await;
    let mut draft_rule = rule(vec![yt]);
    draft_rule.output = RuleOutput::Draft;
    h.app.create_rule(c, draft_rule).await.unwrap();
    let asset = imported(&h, "a.mp4").await;
    h.app.add_to_collection(c, asset).await.unwrap();
    let p = publications(&h, asset).await.remove(0);
    assert_eq!(p.state(), PublicationState::Draft);
    assert!(p.schedule().is_none());
}

#[tokio::test]
async fn watch_folder_imports_are_routed_and_rules_fire_once() {
    let h = harness().await;
    posting_slots(&h).await;
    let yt = account(&h, Platform::YoutubeShorts, "Channel").await;
    let c = collection(&h, "From phone").await;
    h.app.create_rule(c, rule(vec![yt])).await.unwrap();
    let prefs = h
        .app
        .update_preferences(PreferencesUpdate {
            watch_collection: Some(Some(c)),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(prefs.watch_collection, Some(c));
    assert!(h.app.collection(c).await.unwrap().watch_folder_target);

    let path = h.video("phone clip.mp4", b"clip from the phone");
    let ImportOutcome::Imported { asset_id } = h.app.import_watched(&path).await.unwrap() else {
        panic!("expected a new import")
    };
    assert_eq!(publications(&h, asset_id).await.len(), 1);

    // The same file again (re-synced, or a copy under another name): a duplicate, routed
    // again, but no second publication.
    let copy = h.video("phone clip (1).mp4", b"clip from the phone");
    assert_eq!(
        h.app.import_watched(&copy).await.unwrap(),
        ImportOutcome::Duplicate { asset_id }
    );
    assert_eq!(h.app.apply_rules(asset_id).await.unwrap(), vec![]);
    // Leaving and re-entering the collection doesn't fire again either.
    h.app.remove_from_collection(c, asset_id).await.unwrap();
    assert_eq!(h.app.add_to_collection(c, asset_id).await.unwrap(), vec![]);
    assert_eq!(publications(&h, asset_id).await.len(), 1);

    // Deleting the routed collection turns routing off.
    h.app.delete_collection(c).await.unwrap();
    assert_eq!(h.app.preferences().await.unwrap().watch_collection, None);
    let other = h.video("other.mp4", b"another clip");
    assert!(matches!(
        h.app.import_watched(&other).await.unwrap(),
        ImportOutcome::Imported { .. }
    ));
}

#[tokio::test]
async fn saved_links_fire_when_their_media_arrives() {
    let h = harness().await;
    posting_slots(&h).await;
    let yt = account(&h, Platform::YoutubeShorts, "Channel").await;
    let c = collection(&h, "Links").await;
    h.app.create_rule(c, rule(vec![yt])).await.unwrap();
    let link = h
        .app
        .save_link("https://www.tiktok.com/@baker/video/1", Some("Bagels"))
        .await
        .unwrap();
    assert_eq!(h.app.add_to_collection(c, link).await.unwrap(), vec![]);
    assert!(publications(&h, link).await.is_empty());

    let path = h.video("bagels.mp4", b"bagel bytes");
    h.app.import_file(&path, Some(link)).await.unwrap();
    let pubs = publications(&h, link).await;
    assert_eq!(pubs.len(), 1);
    assert_eq!(pubs[0].content().caption, "Bagels 🍞");
}

#[tokio::test]
async fn disabled_rules_and_other_collections_do_nothing() {
    let h = harness().await;
    let yt = account(&h, Platform::YoutubeShorts, "Channel").await;
    let c = collection(&h, "Paused").await;
    let mut paused = rule(vec![yt]);
    paused.enabled = false;
    let view = h.app.create_rule(c, paused).await.unwrap();
    let asset = imported(&h, "x.mp4").await;
    assert_eq!(h.app.add_to_collection(c, asset).await.unwrap(), vec![]);

    // Re-enabling doesn't backfill assets that are already in the collection.
    let mut enabled = rule(vec![yt]);
    enabled.name = "Now on".into();
    h.app
        .update_rule(view.rules[0].id.parse().unwrap(), enabled)
        .await
        .unwrap();
    let second = imported(&h, "y.mp4").await;
    assert_eq!(h.app.add_to_collection(c, second).await.unwrap().len(), 1);
    assert!(publications(&h, asset).await.is_empty());

    assert!(h.app.create_collection("paused").await.is_err());
    assert!(h.app.create_rule(c, rule(vec![])).await.is_err());
    assert!(
        h.app
            .create_rule(c, rule(vec![AccountId::new()]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unattended_publishing_is_off_by_default_and_refused_for_export_accounts() {
    let h = harness().await;
    let manual = account(&h, Platform::InstagramReels, "Reels").await;
    let c = collection(&h, "Auto").await;
    let view = h.app.collection(c).await.unwrap();
    let entry = view
        .unattended
        .iter()
        .find(|u| u.account_id == manual.to_string())
        .unwrap();
    assert!(!entry.enabled);
    assert!(!entry.available);
    assert!(entry.unavailable_reason.is_some());
    let err = h.app.set_unattended(c, manual, true).await.unwrap_err();
    assert_eq!(err.kind(), "invalid");

    // Even a grant written behind the app's back doesn't bypass approval for such accounts.
    h.app
        .store()
        .collections()
        .set_grant(&shorts_domain::UnattendedGrant {
            collection_id: c,
            account_id: manual,
            enabled_at: t0(),
            enabled_by: "owner".into(),
        })
        .await
        .unwrap();
    h.app.create_rule(c, rule(vec![manual])).await.unwrap();
    let asset = imported(&h, "m.mp4").await;
    grant_permissions(&h, asset).await;
    h.app.add_to_collection(c, asset).await.unwrap();
    h.drain().await;
    let p = publications(&h, asset).await.remove(0);
    assert_eq!(p.state(), PublicationState::AwaitingApproval);
    assert!(!p.has_valid_approval());
}

async fn unattended_harness() -> (Harness, AccountId, CollectionId) {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 1)));
    let h = harness_with(registry).await;
    posting_slots(&h).await;
    let auto = automatic_account(&h, Platform::YoutubeShorts).await;
    let c = collection(&h, "Unattended").await;
    h.app.create_rule(c, rule(vec![auto])).await.unwrap();
    (h, auto, c)
}

#[tokio::test]
async fn an_explicit_grant_lets_the_rule_approve_once_the_render_is_ready() {
    let (h, auto, c) = unattended_harness().await;
    let view = h.app.set_unattended(c, auto, true).await.unwrap();
    assert!(view.unattended.iter().any(|u| u.enabled));
    assert!(view.rules[0].destinations[0].unattended);

    let asset = imported(&h, "auto.mp4").await;
    grant_permissions(&h, asset).await;
    let created = h.app.add_to_collection(c, asset).await.unwrap();
    let id: PublicationId = created[0];
    // Not approved until the media it would publish is rendered and reviewable.
    let p = h.app.store().publications().require(id).await.unwrap();
    assert_eq!(p.state(), PublicationState::AwaitingApproval);

    h.drain().await;
    let p = h.app.store().publications().require(id).await.unwrap();
    assert_eq!(p.state(), PublicationState::Scheduled);
    let approval = p.approval().unwrap();
    assert_eq!(approval.approved_by, "rule:Cross-post");
    assert_eq!(approval.revision, 1);
    assert_eq!(publish_jobs(&h).await, 1);

    // The rule approves a publication only once: after the user revokes, it stays revoked.
    h.app.revoke_approval(id).await.unwrap();
    h.app.render_variant(p.content().variant_id).await.unwrap();
    h.drain().await;
    let p = h.app.store().publications().require(id).await.unwrap();
    assert_eq!(p.state(), PublicationState::AwaitingApproval);
}

#[tokio::test]
async fn approval_stays_required_when_the_grant_is_withdrawn_or_review_has_issues() {
    let (h, auto, c) = unattended_harness().await;
    h.app.set_unattended(c, auto, true).await.unwrap();

    // Withdrawn before the render finished.
    let first = imported(&h, "first.mp4").await;
    grant_permissions(&h, first).await;
    let [a] = h.app.add_to_collection(c, first).await.unwrap()[..] else {
        panic!()
    };
    h.app.set_unattended(c, auto, false).await.unwrap();
    h.drain().await;
    let p = h.app.store().publications().require(a).await.unwrap();
    assert_eq!(p.state(), PublicationState::AwaitingApproval);

    // Audio permission not recorded: the review shows an issue, so the user decides.
    h.app.set_unattended(c, auto, true).await.unwrap();
    let second = imported(&h, "second.mp4").await;
    let [b] = h.app.add_to_collection(c, second).await.unwrap()[..] else {
        panic!()
    };
    h.drain().await;
    let p = h.app.store().publications().require(b).await.unwrap();
    assert_eq!(p.state(), PublicationState::AwaitingApproval);
    assert!(!p.has_valid_approval());
}

#[tokio::test]
async fn rules_ignore_the_back_catalog_and_say_when_they_started() {
    let h = harness().await;
    posting_slots(&h).await;
    let yt = account(&h, Platform::YoutubeShorts, "Channel").await;
    let c = collection(&h, "Phone").await;
    // Already in the collection before the rule existed.
    let old = imported(&h, "old.mp4").await;
    h.app.add_to_collection(c, old).await.unwrap();

    h.clock.advance(chrono::Duration::hours(1));
    let view = h.app.create_rule(c, rule(vec![yt])).await.unwrap();
    let r = &view.rules[0];
    assert_eq!(r.effective_from, t0() + chrono::Duration::hours(1));
    assert!(
        r.summary.contains("on or after Fri 2026-10-30 09:00 EDT"),
        "{}",
        r.summary
    );
    assert!(r.summary.contains("YouTube Shorts · Channel"));
    assert!(r.summary.contains("waiting for your approval"));

    // Running the rules again (e.g. after a re-import) still skips the old member.
    assert_eq!(h.app.apply_rules(old).await.unwrap(), vec![]);
    let new = imported(&h, "new.mp4").await;
    assert_eq!(h.app.add_to_collection(c, new).await.unwrap().len(), 1);
    let link = h
        .app
        .save_link("https://www.tiktok.com/@baker/video/9", Some("Pending"))
        .await
        .unwrap();
    h.app.add_to_collection(c, link).await.unwrap();

    // The rule's activity: the pending link waiting for its file, then what it created.
    let activity = h.app.rule_activity(r.id.parse().unwrap()).await.unwrap();
    assert_eq!(activity.items.len(), 2);
    assert!(activity.items[0].waiting_for_media);
    assert_eq!(activity.items[0].title, "Pending");
    assert_eq!(activity.items[1].asset_id, new.to_string());
    let p = &activity.items[1].publications[0];
    assert_eq!(p.state, PublicationState::AwaitingApproval);
    assert!(p.actions.contains(&shorts_desktop::views::Action::Approve));
    assert_eq!(activity.items[1].duration_ms, Some(20_000));
}
