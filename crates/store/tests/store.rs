//! Store tests against temporary on-disk databases.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use shorts_domain::{
    Asset, AssetStatus, AttemptOutcome, Checksum, Collection, CollectionRule, ConnectedAccount,
    Destination, ExecutionOwner, HandoverId, InFlight, Job, JobKind, JobState, MediaFile,
    MediaFileId, MediaProperties, OverduePolicy, PermissionStatus, Platform, Publication,
    PublicationEvent, PublicationState, PublishAttempt, Rating, RenderRecipe, RevisionContent,
    RuleFiring, RuleId, RuleOutput, RuleSpec, Schedule, Tag, TikTokPost, TimeZoneName,
    TransitionError, UnattendedGrant, Variant, Workflow, WorkflowSettings,
};
use shorts_store::{AssetFilter, JobFilter, PublicationFilter, Store, StoreError};
use tempfile::TempDir;

async fn store() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("library.db")).await.unwrap();
    (dir, store)
}

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-17T09:00:00.123456789Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn checksum(seed: u8) -> Checksum {
    Checksum::parse(&format!("{seed:02x}").repeat(32)).unwrap()
}

fn props() -> MediaProperties {
    MediaProperties {
        duration_ms: 20_000,
        width: 1080,
        height: 1920,
        audio_streams: 1,
        video_codec: Some("h264".into()),
        ..Default::default()
    }
}

struct Fixture {
    asset: Asset,
    media: MediaFile,
    variant: Variant,
    account: ConnectedAccount,
}

async fn fixture(store: &Store, seed: u8) -> Fixture {
    let asset = Asset::imported(&format!("Clip {seed}"), t0()).unwrap();
    store.assets().insert(&asset).await.unwrap();
    let media = MediaFile {
        id: MediaFileId::new(),
        asset_id: asset.id,
        path: format!("originals/{seed}.mp4"),
        original_name: format!("{seed}.mp4"),
        checksum: checksum(seed),
        size_bytes: 1234,
        properties: props(),
        imported_at: t0(),
    };
    store.media_files().insert(&media).await.unwrap();
    let variant = Variant::new(
        asset.id,
        media.id,
        "Vertical",
        RenderRecipe::default(),
        &props(),
        t0(),
    )
    .unwrap();
    store.variants().insert(&variant).await.unwrap();
    let account = ConnectedAccount::manual(Platform::InstagramReels, "Test account", t0());
    store.accounts().insert(&account).await.unwrap();
    Fixture {
        asset,
        media,
        variant,
        account,
    }
}

fn draft(f: &Fixture) -> Publication {
    let content = RevisionContent::new(
        f.variant.id,
        f.variant.revision(),
        "Caption",
        &["reels".into()],
        Destination {
            account_id: f.account.id,
            platform: f.account.platform,
        },
    )
    .unwrap();
    Publication::draft(
        f.asset.id,
        content,
        Some(Schedule::new(
            t0() + Duration::hours(1),
            TimeZoneName::parse("Europe/London").unwrap(),
        )),
        t0(),
    )
}

#[tokio::test]
async fn opens_in_wal_mode_and_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    let store = Store::open(&path).await.unwrap();
    assert_eq!(store.journal_mode().await.unwrap(), "wal");
    let asset = Asset::imported("Kept", t0()).unwrap();
    store.assets().insert(&asset).await.unwrap();
    store.close().await;

    let reopened = Store::open(&path).await.unwrap();
    assert_eq!(reopened.assets().require(asset.id).await.unwrap(), asset);
}

#[tokio::test]
async fn assets_round_trip_with_tags_and_permissions() {
    let (_dir, store) = store().await;
    let mut asset = Asset::imported("Pasta night", t0()).unwrap();
    asset.creator = Some("Chef".into());
    asset.add_tag(Tag::parse("food").unwrap(), t0());
    asset.rate(Some(Rating::new(4).unwrap()), t0());
    asset.record_audio_permission(
        PermissionStatus::Denied,
        Some("licensed sound".into()),
        t0(),
    );
    store.assets().insert(&asset).await.unwrap();
    assert_eq!(store.assets().require(asset.id).await.unwrap(), asset);

    asset.add_tag(Tag::parse("dinner").unwrap(), t0());
    asset.remove_tag(&Tag::parse("food").unwrap(), t0());
    asset.notes = "remake in spring".into();
    store.assets().update(&asset).await.unwrap();
    assert_eq!(store.assets().require(asset.id).await.unwrap(), asset);
}

#[tokio::test]
async fn library_search_filters() {
    let (_dir, store) = store().await;
    let mut a = Asset::imported("Sourdough 100% rye", t0()).unwrap();
    a.add_tag(Tag::parse("bread").unwrap(), t0());
    a.rate(Some(Rating::new(5).unwrap()), t0());
    let mut b = Asset::imported("Morning run", t0() + Duration::seconds(1)).unwrap();
    b.add_tag(Tag::parse("fitness").unwrap(), t0());
    b.add_tag(Tag::parse("bread").unwrap(), t0());
    b.rate(Some(Rating::new(2).unwrap()), t0());
    let c = Asset::from_link(
        "Saved link",
        "https://www.tiktok.com/@someone/video/1",
        t0() + Duration::seconds(2),
    )
    .unwrap();
    for asset in [&a, &b, &c] {
        store.assets().insert(asset).await.unwrap();
    }
    let titles = |assets: Vec<Asset>| assets.into_iter().map(|a| a.title).collect::<Vec<_>>();
    let list = |filter: AssetFilter| {
        let store = store.clone();
        async move { titles(store.assets().list(&filter).await.unwrap()) }
    };

    assert_eq!(
        list(AssetFilter::default()).await,
        vec!["Saved link", "Morning run", "Sourdough 100% rye"]
    );
    assert_eq!(
        list(AssetFilter {
            text: Some("SOURDOUGH".into()),
            ..Default::default()
        })
        .await,
        vec!["Sourdough 100% rye"]
    );
    // `%` is matched literally, not as a wildcard.
    assert_eq!(
        list(AssetFilter {
            text: Some("100%".into()),
            ..Default::default()
        })
        .await,
        vec!["Sourdough 100% rye"]
    );
    assert_eq!(
        list(AssetFilter {
            text: Some("#fitness".into()),
            ..Default::default()
        })
        .await,
        vec!["Morning run"]
    );
    assert_eq!(
        list(AssetFilter {
            text: Some("tiktok".into()),
            ..Default::default()
        })
        .await,
        vec!["Saved link"]
    );
    assert_eq!(
        list(AssetFilter {
            tags: vec![Tag::parse("bread").unwrap(), Tag::parse("fitness").unwrap()],
            ..Default::default()
        })
        .await,
        vec!["Morning run"]
    );
    assert_eq!(
        list(AssetFilter {
            min_rating: Some(Rating::new(3).unwrap()),
            ..Default::default()
        })
        .await,
        vec!["Sourdough 100% rye"]
    );
    assert_eq!(
        list(AssetFilter {
            status: Some(AssetStatus::AwaitingMedia),
            ..Default::default()
        })
        .await,
        vec!["Saved link"]
    );
    let counts = store.assets().tag_counts().await.unwrap();
    assert_eq!(counts[0], (Tag::parse("bread").unwrap(), 2));
}

#[tokio::test]
async fn duplicate_media_is_detected_and_originals_are_immutable() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 1).await;
    let found = store
        .media_files()
        .find_by_checksum(&f.media.checksum)
        .await
        .unwrap();
    assert_eq!(found.as_ref(), Some(&f.media));

    let copy = MediaFile {
        id: MediaFileId::new(),
        ..f.media.clone()
    };
    assert!(matches!(
        store.media_files().insert(&copy).await,
        Err(StoreError::DuplicateMedia { .. })
    ));

    let tamper = sqlx::query("UPDATE media_files SET path = 'elsewhere' WHERE id = ?")
        .bind(f.media.id.to_string())
        .execute(store.pool())
        .await;
    assert!(tamper.is_err(), "media_files rows must be immutable");
}

#[tokio::test]
async fn variant_updates_use_the_loaded_revision() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 2).await;
    let loaded = store.variants().require(f.variant.id).await.unwrap();
    let mut edit = loaded.clone();
    edit.update_recipe(
        RenderRecipe {
            audio: shorts_domain::AudioChoice::Mute,
            ..Default::default()
        },
        &props(),
        t0(),
    )
    .unwrap();
    store
        .variants()
        .update(&edit, loaded.revision())
        .await
        .unwrap();

    // A writer still holding revision 1 (e.g. a slow render) loses.
    let mut stale = loaded.clone();
    stale.rename("Stale", t0()).unwrap();
    assert!(matches!(
        store.variants().update(&stale, loaded.revision()).await,
        Err(StoreError::Conflict { .. })
    ));
    let current = store.variants().require(f.variant.id).await.unwrap();
    assert_eq!(current.revision(), 2);
    assert_eq!(
        store.variants().for_asset(f.asset.id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn accounts_round_trip() {
    let (_dir, store) = store().await;
    let mut account = ConnectedAccount::manual(Platform::Linkedin, "Company page", t0());
    store.accounts().insert(&account).await.unwrap();
    account.display_name = "Renamed".into();
    store.accounts().update(&account).await.unwrap();
    assert_eq!(store.accounts().list().await.unwrap(), vec![account]);
}

#[tokio::test]
async fn publication_transitions_persist_with_history() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 3).await;
    let mut p = draft(&f);
    store.publications().insert(&p).await.unwrap();
    assert_eq!(store.publications().require(p.id()).await.unwrap(), p);

    let t = p.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    store.publications().save(&p, &t).await.unwrap();
    let t = p
        .apply(
            PublicationEvent::Approve {
                revision: 1,
                current_variant_revision: 1,
                by: "owner".into(),
            },
            t0(),
        )
        .unwrap();
    store.publications().save(&p, &t).await.unwrap();
    let t = p
        .apply(
            PublicationEvent::Revise {
                content: RevisionContent {
                    caption: "Edited".into(),
                    ..p.content().clone()
                },
            },
            t0() + Duration::minutes(1),
        )
        .unwrap();
    store.publications().save(&p, &t).await.unwrap();

    let loaded = store.publications().require(p.id()).await.unwrap();
    assert_eq!(loaded, p);
    assert_eq!(loaded.state(), PublicationState::AwaitingApproval);
    assert!(loaded.approval().is_none());

    let revisions = store.publications().revisions(p.id()).await.unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].content.caption, "Caption");
    assert_eq!(revisions[1].content.caption, "Edited");

    let log = store.publications().log(p.id()).await.unwrap();
    let events: Vec<_> = log.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(
        events,
        vec!["created", "submit_for_review", "approve", "revise"]
    );
    assert_eq!(log[3].from, Some(PublicationState::Scheduled));
    assert_eq!(log[3].revision, 2);
}

#[tokio::test]
async fn revisions_cannot_be_rewritten() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 4).await;
    let p = draft(&f);
    store.publications().insert(&p).await.unwrap();
    let update = sqlx::query("UPDATE publication_revisions SET content = '{}'")
        .execute(store.pool())
        .await;
    assert!(update.is_err());
    let delete = sqlx::query("DELETE FROM publication_revisions")
        .execute(store.pool())
        .await;
    assert!(delete.is_err());
}

#[tokio::test]
async fn stale_publication_writes_conflict() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 5).await;
    let p = draft(&f);
    store.publications().insert(&p).await.unwrap();

    let mut a = store.publications().require(p.id()).await.unwrap();
    let mut b = a.clone();
    let ta = a.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    let tb = b.apply(PublicationEvent::Cancel, t0()).unwrap();
    store.publications().save(&a, &ta).await.unwrap();
    assert!(matches!(
        store.publications().save(&b, &tb).await,
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(
        store.publications().require(p.id()).await.unwrap().state(),
        PublicationState::AwaitingApproval
    );
}

#[tokio::test]
async fn publications_can_be_filtered() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 6).await;
    let mut a = draft(&f);
    let b = draft(&f);
    store.publications().insert(&a).await.unwrap();
    store.publications().insert(&b).await.unwrap();
    let t = a.apply(PublicationEvent::Cancel, t0()).unwrap();
    store.publications().save(&a, &t).await.unwrap();

    let drafts = store
        .publications()
        .list(&PublicationFilter {
            states: vec![PublicationState::Draft],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        drafts.iter().map(|p| p.id()).collect::<Vec<_>>(),
        vec![b.id()]
    );
    let for_asset = store
        .publications()
        .list(&PublicationFilter {
            asset_id: Some(f.asset.id),
            account_id: Some(f.account.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(for_asset.len(), 2);
    assert_eq!(
        store
            .publications()
            .using_variant(f.variant.id)
            .await
            .unwrap()
            .len(),
        2
    );
}

fn job(subject: &str, run_at: DateTime<Utc>) -> Job {
    Job::new(
        JobKind::PublishPublication,
        subject,
        json!({"publication": subject}),
        run_at,
        true,
        5,
        t0(),
    )
}

#[tokio::test]
async fn one_open_job_per_subject() {
    let (_dir, store) = store().await;
    store.jobs().insert(&job("p1", t0())).await.unwrap();
    assert!(matches!(
        store.jobs().insert(&job("p1", t0())).await,
        Err(StoreError::DuplicateOpenJob { .. })
    ));
    // A different kind for the same subject is fine.
    let render = Job::new(
        JobKind::RenderVariant,
        "p1",
        json!({}),
        t0(),
        false,
        3,
        t0(),
    );
    store.jobs().insert(&render).await.unwrap();
    assert!(
        store
            .jobs()
            .open_for(JobKind::PublishPublication, "p1")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn claims_take_due_jobs_in_order() {
    let (_dir, store) = store().await;
    let late = job("late", t0() + Duration::minutes(10));
    let first = job("first", t0() - Duration::minutes(5));
    let second = job("second", t0());
    for j in [&late, &first, &second] {
        store.jobs().insert(j).await.unwrap();
    }
    let ttl = Duration::seconds(30);
    let claimed = store
        .jobs()
        .claim_next("desktop:a", t0(), ttl, &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id(), first.id());
    assert_eq!(claimed.state(), JobState::Running);
    let claimed = store
        .jobs()
        .claim_next("desktop:a", t0(), ttl, &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id(), second.id());
    assert!(
        store
            .jobs()
            .claim_next("desktop:a", t0(), ttl, &[])
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.jobs().next_due_at(&[]).await.unwrap(),
        Some(late.run_at())
    );
    // Kind filters are honoured.
    assert!(
        store
            .jobs()
            .claim_next(
                "desktop:a",
                t0() + Duration::hours(1),
                ttl,
                &[JobKind::RenderVariant]
            )
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn concurrent_claimers_never_share_a_job() {
    let (_dir, store) = store().await;
    for i in 0..40 {
        store
            .jobs()
            .insert(&job(&format!("p{i}"), t0()))
            .await
            .unwrap();
    }
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for w in 0..8 {
        let store = store.clone();
        let seen = seen.clone();
        tasks.push(tokio::spawn(async move {
            let owner = format!("desktop:w{w}");
            while let Some(j) = store
                .jobs()
                .claim_next(&owner, t0(), Duration::seconds(60), &[])
                .await
                .unwrap()
            {
                seen.lock().await.push(j.id());
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let seen = seen.lock().await;
    let unique: HashSet<_> = seen.iter().collect();
    assert_eq!(seen.len(), 40);
    assert_eq!(unique.len(), 40);
}

#[tokio::test]
async fn expired_leases_are_reclaimed_and_old_owner_loses_it() {
    let (_dir, store) = store().await;
    let j = job("p1", t0());
    store.jobs().insert(&j).await.unwrap();
    let ttl = Duration::seconds(30);
    store
        .jobs()
        .claim_next("desktop:a", t0(), ttl, &[])
        .await
        .unwrap()
        .unwrap();
    // Heartbeat while the lease is live.
    store
        .jobs()
        .renew(j.id(), "desktop:a", t0() + Duration::seconds(20), ttl)
        .await
        .unwrap();
    assert!(
        store
            .jobs()
            .claim_next("desktop:b", t0() + Duration::seconds(40), ttl, &[])
            .await
            .unwrap()
            .is_none(),
        "renewed lease is still live"
    );
    let reclaimed = store
        .jobs()
        .claim_next("desktop:b", t0() + Duration::seconds(51), ttl, &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.lease().unwrap().owner, "desktop:b");
    assert_eq!(reclaimed.claims(), 2);

    let err = store
        .jobs()
        .renew(j.id(), "desktop:a", t0() + Duration::seconds(52), ttl)
        .await
        .unwrap_err();
    assert!(err.is_lease_lost(), "{err:?}");
    let err = store
        .jobs()
        .save_checkpoint(j.id(), "desktop:a", json!({}), t0())
        .await
        .unwrap_err();
    assert!(err.is_lease_lost());
}

#[tokio::test]
async fn checkpoints_survive_restart_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    let j = job("p1", t0());
    {
        let store = Store::open(&path).await.unwrap();
        store.jobs().insert(&j).await.unwrap();
        store
            .jobs()
            .claim_next("desktop:old", t0(), Duration::seconds(30), &[])
            .await
            .unwrap()
            .unwrap();
        store
            .jobs()
            .save_checkpoint(j.id(), "desktop:old", json!({"uploaded": 3}), t0())
            .await
            .unwrap();
        store.close().await;
    }
    let store = Store::open(&path).await.unwrap();
    // Jobs leased by other nodes are left alone.
    let other = job("p2", t0());
    store.jobs().insert(&other).await.unwrap();
    store
        .jobs()
        .claim_next("cloud:x", t0(), Duration::seconds(30), &[])
        .await
        .unwrap();

    let recovered = store
        .jobs()
        .recover_interrupted("desktop", "desktop:new", t0())
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id(), j.id());
    assert_eq!(recovered[0].state(), JobState::Queued);
    let claimed = store
        .jobs()
        .claim_next("desktop:new", t0(), Duration::seconds(30), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id(), j.id());
    assert_eq!(claimed.checkpoint(), Some(&json!({"uploaded": 3})));
    assert_eq!(
        store.jobs().require(other.id()).await.unwrap().state(),
        JobState::Running
    );
}

#[tokio::test]
async fn cancelling_a_running_job_revokes_the_lease() {
    let (_dir, store) = store().await;
    let j = job("p1", t0());
    store.jobs().insert(&j).await.unwrap();
    store
        .jobs()
        .claim_next("desktop:a", t0(), Duration::seconds(30), &[])
        .await
        .unwrap();
    store
        .jobs()
        .modify(j.id(), |job| job.cancel(t0()))
        .await
        .unwrap();
    let err = store
        .jobs()
        .renew(j.id(), "desktop:a", t0(), Duration::seconds(30))
        .await
        .unwrap_err();
    assert!(err.is_lease_lost());
    // The subject is free for a new job.
    store.jobs().insert(&job("p1", t0())).await.unwrap();
    let all = store
        .jobs()
        .list(&JobFilter {
            subject: Some("p1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn attempts_round_trip() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 7).await;
    let p = draft(&f);
    store.publications().insert(&p).await.unwrap();
    let mut attempt = PublishAttempt::start(p.id(), 1, None, t0());
    store.attempts().insert(&attempt).await.unwrap();
    attempt.mark_publish_requested(t0());
    attempt.finish(
        AttemptOutcome::HandedOff {
            package_dir: "exports/x".into(),
        },
        t0(),
    );
    store.attempts().update(&attempt).await.unwrap();
    assert_eq!(
        store.attempts().for_publication(p.id()).await.unwrap(),
        vec![attempt.clone()]
    );
    assert_eq!(
        store.attempts().get(attempt.id).await.unwrap(),
        Some(attempt)
    );
}

#[tokio::test]
async fn settings_round_trip() {
    let (_dir, store) = store().await;
    assert_eq!(
        store
            .settings()
            .get::<OverduePolicy>("overdue_policy")
            .await
            .unwrap(),
        None
    );
    store
        .settings()
        .set("overdue_policy", &OverduePolicy::CatchUp)
        .await
        .unwrap();
    store
        .settings()
        .set("overdue_policy", &OverduePolicy::Expire { grace_secs: 60 })
        .await
        .unwrap();
    assert_eq!(
        store.settings().get("overdue_policy").await.unwrap(),
        Some(OverduePolicy::Expire { grace_secs: 60 })
    );
    store.settings().remove("overdue_policy").await.unwrap();
    assert_eq!(
        store
            .settings()
            .get::<OverduePolicy>("overdue_policy")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn imports_are_saved_atomically() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 8).await;

    // A second import of the same bytes fails as a whole: no orphan asset or variant.
    let asset = Asset::imported("Copy", t0()).unwrap();
    let media = MediaFile {
        id: MediaFileId::new(),
        asset_id: asset.id,
        ..f.media.clone()
    };
    let variant = Variant::new(
        asset.id,
        media.id,
        "Vertical",
        RenderRecipe::default(),
        &props(),
        t0(),
    )
    .unwrap();
    assert!(matches!(
        store.save_import(&asset, true, &media, &variant).await,
        Err(StoreError::DuplicateMedia { .. })
    ));
    assert!(store.assets().get(asset.id).await.unwrap().is_none());

    // Media for a saved link updates the existing asset.
    let mut link = Asset::from_link("Saved", "https://example.com/v/2", t0()).unwrap();
    store.assets().insert(&link).await.unwrap();
    link.mark_media_attached(t0());
    let media = MediaFile {
        id: MediaFileId::new(),
        asset_id: link.id,
        checksum: checksum(9),
        ..f.media.clone()
    };
    let variant = Variant::new(
        link.id,
        media.id,
        "Vertical",
        RenderRecipe::default(),
        &props(),
        t0(),
    )
    .unwrap();
    store
        .save_import(&link, false, &media, &variant)
        .await
        .unwrap();
    assert_eq!(
        store.assets().require(link.id).await.unwrap().status,
        AssetStatus::Ready
    );
    assert_eq!(store.variants().for_asset(link.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn latest_job_for_a_subject_includes_closed_jobs() {
    let (_dir, store) = store().await;
    let first = job("p1", t0());
    store.jobs().insert(&first).await.unwrap();
    store
        .jobs()
        .modify(first.id(), |j| j.cancel(t0()))
        .await
        .unwrap();
    let latest = store
        .jobs()
        .latest_for(JobKind::PublishPublication, "p1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id(), first.id());
    assert_eq!(latest.state(), JobState::Cancelled);
    let second = job("p1", t0() + Duration::seconds(1));
    store.jobs().insert(&second).await.unwrap();
    let latest = store
        .jobs()
        .latest_for(JobKind::PublishPublication, "p1")
        .await
        .unwrap()
        .unwrap();
    // Job ids are time-ordered, and the second was created later.
    assert_eq!(latest.id(), second.id());
}

#[tokio::test]
async fn collections_hold_members_rules_and_grants() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 40).await;
    let c = Collection::new("Best of", t0()).unwrap();
    store.collections().insert(&c).await.unwrap();
    let clash = Collection::new("best OF", t0()).unwrap();
    assert!(matches!(
        store.collections().insert(&clash).await,
        Err(StoreError::DuplicateName { .. })
    ));

    assert!(
        store
            .collections()
            .add_member(c.id, f.asset.id, t0())
            .await
            .unwrap()
    );
    assert!(
        !store
            .collections()
            .add_member(c.id, f.asset.id, t0())
            .await
            .unwrap()
    );
    assert_eq!(
        store.collections().members(c.id).await.unwrap(),
        vec![f.asset.id]
    );
    assert_eq!(
        store.collections().of_asset(f.asset.id).await.unwrap(),
        vec![c.clone()]
    );
    assert_eq!(store.collections().list().await.unwrap()[0].member_count, 1);

    let rule = CollectionRule::new(
        c.id,
        RuleSpec {
            name: "Cross-post".into(),
            enabled: true,
            destinations: vec![f.account.id],
            output: RuleOutput::AwaitingApproval,
            caption_template: "{title}".into(),
            hashtags: vec![],
            include_asset_tags: false,
        },
        None,
        t0(),
    )
    .unwrap();
    store.collections().insert_rule(&rule).await.unwrap();
    assert_eq!(
        store
            .collections()
            .member_since(c.id, f.asset.id)
            .await
            .unwrap(),
        Some(t0())
    );
    assert_eq!(
        store.collections().rules(c.id).await.unwrap(),
        vec![rule.clone()]
    );

    assert!(
        store
            .collections()
            .grant(c.id, f.account.id)
            .await
            .unwrap()
            .is_none()
    );
    let grant = UnattendedGrant {
        collection_id: c.id,
        account_id: f.account.id,
        enabled_at: t0(),
        enabled_by: "owner".into(),
    };
    store.collections().set_grant(&grant).await.unwrap();
    assert_eq!(
        store.collections().grant(c.id, f.account.id).await.unwrap(),
        Some(grant)
    );

    // Deleting the collection removes rules, grants and memberships, not the asset.
    store.collections().delete(c.id).await.unwrap();
    assert!(store.collections().rule(rule.id).await.unwrap().is_none());
    assert!(
        store
            .collections()
            .of_asset(f.asset.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.assets().get(f.asset.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_rule_fires_once_per_asset_and_account() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 41).await;
    let rule_id = RuleId::new();
    let first = draft(&f);
    let firing = RuleFiring {
        rule_id,
        asset_id: f.asset.id,
        account_id: f.account.id,
        publication_id: first.id(),
        unattended: true,
        auto_approved_at: None,
        fired_at: t0(),
    };
    assert!(store.record_rule_firing(&firing, &first).await.unwrap());
    assert!(
        store
            .collections()
            .firing_exists(rule_id, f.asset.id, f.account.id)
            .await
            .unwrap()
    );

    // A second firing for the same triple writes nothing, not even the publication.
    let second = draft(&f);
    let again = RuleFiring {
        publication_id: second.id(),
        ..firing.clone()
    };
    assert!(!store.record_rule_firing(&again, &second).await.unwrap());
    assert!(
        store
            .publications()
            .get(second.id())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(store.collections().fired_count(rule_id).await.unwrap(), 1);

    assert!(
        store
            .collections()
            .mark_auto_approved(first.id(), t0())
            .await
            .unwrap()
    );
    assert!(
        !store
            .collections()
            .mark_auto_approved(first.id(), t0())
            .await
            .unwrap()
    );
    let stored = store
        .collections()
        .firing_for_publication(first.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.auto_approved_at, Some(t0()));
}

fn tiktok_post(asset: &Asset, video_id: &str) -> TikTokPost {
    TikTokPost {
        video_id: video_id.into(),
        asset_id: asset.id,
        posted_at: t0(),
        duration_ms: Some(20_000),
        title: asset.title.clone(),
        caption: "caption #bread".into(),
        share_url: Some(format!("https://www.tiktok.com/@test/video/{video_id}")),
        cover_url: None,
        cover_path: None,
        discovered_at: t0(),
        matched_at: None,
    }
}

#[tokio::test]
async fn tiktok_posts_are_recorded_once_and_merged_into_the_imported_file() {
    let (_dir, store) = store().await;
    let item = Asset::awaiting_media("Bagels", Some("https://www.tiktok.com/@test/video/1"), t0())
        .unwrap();
    let post = tiktok_post(&item, "1");
    assert!(store.record_tiktok_post(&item, &post).await.unwrap());
    // The same post again writes nothing (not even a second item).
    let again = Asset::awaiting_media("Bagels", None, t0()).unwrap();
    assert!(
        !store
            .record_tiktok_post(&again, &tiktok_post(&again, "1"))
            .await
            .unwrap()
    );
    assert!(store.assets().get(again.id).await.unwrap().is_none());
    assert_eq!(
        store.tiktok_posts().pending().await.unwrap(),
        vec![post.clone()]
    );

    let c = Collection::new("Phone", t0()).unwrap();
    store.collections().insert(&c).await.unwrap();
    store
        .collections()
        .add_member(c.id, item.id, t0())
        .await
        .unwrap();

    // Refused while the "awaiting" item actually has media.
    let f = fixture(&store, 50).await;
    assert!(
        store
            .merge_tiktok_match("1", f.asset.id, &f.asset, t0())
            .await
            .is_err()
    );

    let mut target = f.asset.clone();
    target.title = "Bagels".into();
    store
        .merge_tiktok_match("1", item.id, &target, t0())
        .await
        .unwrap();
    assert!(store.assets().get(item.id).await.unwrap().is_none());
    assert_eq!(
        store.assets().require(f.asset.id).await.unwrap().title,
        "Bagels"
    );
    let moved = store.tiktok_posts().require("1").await.unwrap();
    assert_eq!(moved.asset_id, f.asset.id);
    assert_eq!(moved.matched_at, Some(t0()));
    assert!(store.tiktok_posts().pending().await.unwrap().is_empty());
    assert_eq!(
        store.collections().members(c.id).await.unwrap(),
        vec![f.asset.id]
    );
    assert!(
        store
            .tiktok_posts()
            .linked_assets()
            .await
            .unwrap()
            .contains(&f.asset.id)
    );

    store
        .tiktok_posts()
        .dismiss("1", f.asset.id, t0())
        .await
        .unwrap();
    store
        .tiktok_posts()
        .dismiss("1", f.asset.id, t0())
        .await
        .unwrap();
    assert_eq!(store.tiktok_posts().dismissed().await.unwrap().len(), 1);
    let window = store
        .media_files()
        .imported_between(t0() - Duration::hours(1), t0() + Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(window.len(), 1);
}

#[tokio::test]
async fn quota_spend_accumulates_per_bucket_and_day() {
    let (_dir, store) = store().await;
    let q = store.quota();
    let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
    let next = day.succ_opt().unwrap();
    assert_eq!(q.used("youtube:units", day).await.unwrap(), 0);
    assert_eq!(q.spend("youtube:units", day, 1).await.unwrap(), 1);
    assert_eq!(q.spend("youtube:units", day, 2).await.unwrap(), 3);
    assert_eq!(q.spend("youtube:uploads", day, 1).await.unwrap(), 1);
    assert_eq!(q.spend("youtube:units", next, 5).await.unwrap(), 5);
    assert_eq!(q.used("youtube:units", day).await.unwrap(), 3);

    q.exhaust("youtube:units", day, 10_000).await.unwrap();
    assert_eq!(q.used("youtube:units", day).await.unwrap(), 10_000);
    // Exhausting never lowers a count.
    q.exhaust("youtube:units", day, 50).await.unwrap();
    assert_eq!(q.used("youtube:units", day).await.unwrap(), 10_000);

    assert_eq!(q.prune_before(next).await.unwrap(), 2);
    assert_eq!(q.used("youtube:units", next).await.unwrap(), 5);
}

// --- execution ownership --------------------------------------------------------------------

/// An approved publication with its queued publish job, due at `t0`.
async fn scheduled_with_job(store: &Store, f: &Fixture) -> (Publication, Job) {
    let mut p = draft(f);
    store.publications().insert(&p).await.unwrap();
    for event in [
        PublicationEvent::SubmitForReview,
        PublicationEvent::Approve {
            revision: 1,
            current_variant_revision: 1,
            by: "owner".into(),
        },
    ] {
        let t = p.apply(event, t0()).unwrap();
        store.publications().save(&p, &t).await.unwrap();
    }
    let job = job(&p.id().to_string(), t0());
    store.jobs().insert(&job).await.unwrap();
    (p, job)
}

#[tokio::test]
async fn handing_over_moves_ownership_and_cancels_the_desktop_job() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 21).await;
    let (p, job) = scheduled_with_job(&store, &f).await;

    let handover = HandoverId::new();
    let moved = store
        .hand_over_to_cloud(p.id(), handover, t0(), |_| false)
        .await
        .unwrap();
    assert_eq!(moved.owner(), ExecutionOwner::Cloud);
    assert_eq!(moved.handover(), Some(handover));
    let loaded = store.publications().require(p.id()).await.unwrap();
    assert_eq!(loaded, moved);
    assert_eq!(
        store.jobs().require(job.id()).await.unwrap().state(),
        JobState::Cancelled
    );
    let log = store.publications().log(p.id()).await.unwrap();
    assert_eq!(log.last().unwrap().event, "hand_to_cloud");
    let cloud = store
        .publications()
        .list(&PublicationFilter {
            owner: Some(ExecutionOwner::Cloud),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(cloud.len(), 1);

    // Even a publish job enqueued afterwards is never claimed on the desktop.
    store
        .jobs()
        .insert(&self::job(&p.id().to_string(), t0()))
        .await
        .unwrap();
    assert!(
        store
            .jobs()
            .claim_next("desktop:w", t0(), Duration::seconds(60), &[])
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn handing_over_is_refused_while_the_desktop_job_runs_or_reached_the_platform() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 22).await;
    let (p, job) = scheduled_with_job(&store, &f).await;

    let err = store
        .hand_over_to_cloud(p.id(), HandoverId::new(), t0(), |_| true)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::Transition(TransitionError::InFlight(InFlight::ReachedProvider))
    ));

    let claimed = store
        .jobs()
        .claim_next("desktop:w", t0(), Duration::seconds(60), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id(), job.id());
    let err = store
        .hand_over_to_cloud(p.id(), HandoverId::new(), t0(), |_| false)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::Transition(TransitionError::InFlight(InFlight::JobRunning))
    ));
    let loaded = store.publications().require(p.id()).await.unwrap();
    assert_eq!(loaded.owner(), ExecutionOwner::Desktop);
    let still = store.jobs().require(job.id()).await.unwrap();
    assert_eq!(still.state(), JobState::Running);
    assert_eq!(still.lease().unwrap().owner, "desktop:w");
}

/// The desktop worker claiming a job and the user handing its publication to the cloud race;
/// exactly one wins, every time.
#[tokio::test]
async fn claiming_and_handing_over_never_both_win() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 23).await;
    let (mut claims, mut handovers) = (0, 0);
    for _ in 0..30 {
        let (p, _) = scheduled_with_job(&store, &f).await;
        let id = p.id();
        let claimer = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .jobs()
                    .claim_next("desktop:w", t0(), Duration::seconds(60), &[])
                    .await
                    .unwrap()
            })
        };
        let mover = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .hand_over_to_cloud(id, HandoverId::new(), t0(), |_| false)
                    .await
            })
        };
        let claimed = claimer.await.unwrap();
        let moved = mover.await.unwrap();
        match (claimed, moved) {
            (Some(job), Err(StoreError::Transition(TransitionError::InFlight(_)))) => {
                assert_eq!(job.subject(), p.id().to_string());
                claims += 1;
                // Finish it so the next round's claim only sees the new job.
                store
                    .jobs()
                    .modify(job.id(), |j| j.succeed("desktop:w", t0()))
                    .await
                    .unwrap();
            }
            (None, Ok(moved)) => {
                assert_eq!(moved.owner(), ExecutionOwner::Cloud);
                handovers += 1;
            }
            (claimed, moved) => panic!("both or neither won: {claimed:?} / {moved:?}"),
        }
    }
    assert_eq!(claims + handovers, 30);
}

#[tokio::test]
async fn workflows_wrap_a_rule_and_grant_and_go_with_them() {
    let (_dir, store) = store().await;
    let f = fixture(&store, 41).await;
    let c = Collection::new("Source videos", t0()).unwrap();
    store.collections().insert(&c).await.unwrap();
    let spec = RuleSpec {
        name: "TikTok → Instagram Reels".into(),
        enabled: true,
        destinations: vec![f.account.id],
        output: RuleOutput::AwaitingApproval,
        caption_template: String::new(),
        hashtags: vec![],
        include_asset_tags: false,
    };
    let rule = CollectionRule::new(c.id, spec.clone(), None, t0()).unwrap();
    let workflow = Workflow::new(
        rule.id,
        c.id,
        f.account.id,
        WorkflowSettings::default(),
        true,
        t0(),
    );
    let grant = UnattendedGrant {
        collection_id: c.id,
        account_id: f.account.id,
        enabled_at: t0(),
        enabled_by: "owner".into(),
    };
    store
        .workflows()
        .create(&rule, &workflow, Some(&grant))
        .await
        .unwrap();
    assert_eq!(
        store.workflows().list().await.unwrap(),
        vec![workflow.clone()]
    );
    assert_eq!(
        store.workflows().for_rule(rule.id).await.unwrap(),
        Some(workflow.clone())
    );
    assert_eq!(
        store.collections().rules(c.id).await.unwrap(),
        vec![rule.clone()]
    );
    assert!(
        store
            .collections()
            .grant(c.id, f.account.id)
            .await
            .unwrap()
            .is_some()
    );

    // One workflow per collection and account; the failed attempt writes nothing.
    let second = CollectionRule::new(c.id, spec, None, t0()).unwrap();
    let dup = Workflow::new(
        second.id,
        c.id,
        f.account.id,
        WorkflowSettings::default(),
        false,
        t0(),
    );
    assert!(matches!(
        store.workflows().create(&second, &dup, None).await,
        Err(StoreError::DuplicateWorkflow { .. })
    ));
    assert_eq!(store.collections().rules(c.id).await.unwrap().len(), 1);

    let mut edited = workflow.clone();
    edited.confirm_rights(false, t0() + Duration::minutes(1));
    store.workflows().update(&edited).await.unwrap();
    assert_eq!(
        store.workflows().require(workflow.id).await.unwrap(),
        edited
    );

    store.workflows().delete(workflow.id).await.unwrap();
    assert!(store.workflows().get(workflow.id).await.unwrap().is_none());
    assert!(store.collections().rule(rule.id).await.unwrap().is_none());
    assert!(
        store
            .collections()
            .grant(c.id, f.account.id)
            .await
            .unwrap()
            .is_none()
    );
}
