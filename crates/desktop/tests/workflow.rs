//! The desktop workflow end to end, with a fake media backend (no FFmpeg needed).

mod support;

use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, NaiveTime, Weekday};
use shorts_adapters::AdapterError;
use shorts_adapters::AdapterRegistry;
use shorts_adapters::fake::{Call, FakeAdapter};
use shorts_desktop::views::Action;
use shorts_desktop::{
    AssetUpdate, EditPublication, ImportOutcome, LibraryQuery, NewPublication, PermissionInput,
    PreferencesUpdate, ScheduleInput,
};
use shorts_domain::{
    AccountId, AssetId, AttemptOutcome, AudioChoice, Capabilities, ConnectedAccount,
    OutcomeResolution, OverduePolicy, PermissionStatus, Platform, PublicationId, PublicationState,
    PublishAccess, RenderRecipe, ValidationIssue, VariantId,
};
use shorts_scheduler::{Disposition, PostingSlot};
use support::{Harness, harness, harness_with, t0};

async fn imported(h: &Harness, name: &str) -> (AssetId, VariantId) {
    let path = h.video(name, format!("video bytes for {name}").as_bytes());
    let outcome = h.app.import_file(&path, None).await.unwrap();
    let ImportOutcome::Imported { asset_id } = outcome else {
        panic!("expected a new import, got {outcome:?}")
    };
    let detail = h.app.asset_detail(asset_id).await.unwrap();
    (asset_id, detail.variants[0].id.parse().unwrap())
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

fn local(at: &str) -> ScheduleInput {
    ScheduleInput::Local {
        local: at.to_owned(),
        timezone: Some("America/New_York".to_owned()),
    }
}

async fn draft(
    h: &Harness,
    asset: AssetId,
    variant: VariantId,
    account: &str,
    when: ScheduleInput,
) -> PublicationId {
    let view = h
        .app
        .create_publication(NewPublication {
            asset_id: asset,
            variant_id: variant,
            account_id: account.parse().unwrap(),
            caption: "Crispy edges every time".into(),
            hashtags: vec!["#bread".into(), "baking".into()],
            schedule: Some(when),
        })
        .await
        .unwrap();
    view.id.parse().unwrap()
}

#[tokio::test]
async fn import_creates_an_immutable_original_and_a_default_variant() {
    let h = harness().await;
    let path = h.video("my_morning-routine.mp4", b"original bytes");
    let ImportOutcome::Imported { asset_id } = h.app.import_file(&path, None).await.unwrap() else {
        panic!()
    };
    let detail = h.app.asset_detail(asset_id).await.unwrap();
    assert_eq!(detail.asset.title, "my morning routine");
    let media = detail.asset.media.unwrap();
    assert_eq!(media.original_name, "my_morning-routine.mp4");
    let managed = Path::new(&media.path);
    assert!(managed.starts_with(&h.app.paths().originals));
    assert!(std::fs::metadata(managed).unwrap().permissions().readonly());
    assert_eq!(std::fs::read(managed).unwrap(), b"original bytes");
    assert_eq!(detail.variants.len(), 1);
    assert_eq!(detail.variants[0].recipe, RenderRecipe::default());

    // Same bytes under another name: a duplicate, not a second asset.
    let copy = h.video("copy.mov", b"original bytes");
    assert_eq!(
        h.app.import_file(&copy, None).await.unwrap(),
        ImportOutcome::Duplicate { asset_id }
    );
    assert_eq!(
        h.app
            .list_assets(&LibraryQuery::default())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn unsupported_and_unreadable_files_are_rejected() {
    let h = harness().await;
    let text = h.video("notes.txt", b"hello");
    assert!(h.app.import_file(&text, None).await.is_err());
    let broken = h.video("broken.mp4", b"bad bytes");
    let err = h.app.import_file(&broken, None).await.unwrap_err();
    assert_eq!(err.kind(), "invalid");
    assert!(
        h.app
            .list_assets(&LibraryQuery::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn saved_links_await_media_until_a_file_is_attached() {
    let h = harness().await;
    let id = h
        .app
        .save_link("https://www.tiktok.com/@baker/video/123", Some("Bagels"))
        .await
        .unwrap();
    let awaiting = h
        .app
        .list_assets(&LibraryQuery {
            awaiting_media: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(awaiting.len(), 1);
    assert!(awaiting[0].media.is_none());
    assert!(h.app.save_link("not a link", None).await.is_err());

    let path = h.video("bagels.mp4", b"bagel bytes");
    assert_eq!(
        h.app.import_file(&path, Some(id)).await.unwrap(),
        ImportOutcome::Imported { asset_id: id }
    );
    let detail = h.app.asset_detail(id).await.unwrap();
    assert_eq!(detail.asset.status, "ready");
    assert_eq!(detail.asset.title, "Bagels");
    assert_eq!(detail.variants.len(), 1);
}

#[tokio::test]
async fn curation_tags_ratings_and_search() {
    let h = harness().await;
    let (a, _) = imported(&h, "sourdough.mp4").await;
    let (b, _) = imported(&h, "run.mp4").await;
    h.app
        .update_asset(
            a,
            AssetUpdate {
                add_tags: vec!["#Bread".into(), "kitchen".into()],
                rating: Some(5),
                creator: Some("Baker Bee".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.app
        .update_asset(
            b,
            AssetUpdate {
                add_tags: vec!["fitness".into()],
                rating: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let titles = |q: LibraryQuery| {
        let app = h.app.clone();
        async move {
            app.list_assets(&q)
                .await
                .unwrap()
                .into_iter()
                .map(|a| a.title)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        titles(LibraryQuery {
            text: Some("baker".into()),
            ..Default::default()
        })
        .await,
        vec!["sourdough"]
    );
    assert_eq!(
        titles(LibraryQuery {
            tags: vec!["bread".into()],
            ..Default::default()
        })
        .await,
        vec!["sourdough"]
    );
    assert_eq!(
        titles(LibraryQuery {
            min_rating: Some(3),
            ..Default::default()
        })
        .await,
        vec!["sourdough"]
    );
    let tags = h.app.tag_counts().await.unwrap();
    assert_eq!(tags.len(), 3);
    let cleared = h
        .app
        .update_asset(
            a,
            AssetUpdate {
                rating: Some(0),
                remove_tags: vec!["kitchen".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.rating, None);
    assert_eq!(cleared.tags, vec!["bread"]);
    assert!(h.app.thumbnail(a).await.unwrap().unwrap().is_file());
}

#[tokio::test]
async fn manual_handoff_from_draft_to_published() {
    let h = harness().await;
    let (asset, variant) = imported(&h, "bread.mp4").await;
    grant_permissions(&h, asset).await;
    let account = h
        .app
        .add_manual_account(Platform::InstagramReels, "Bakery")
        .await
        .unwrap();
    assert!(!account.automatic);
    // 09:00 New York on 2 November 2026 (EST) is 14:00 UTC.
    let id = draft(&h, asset, variant, &account.id, local("2026-11-02T09:00")).await;
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::Draft);
    assert_eq!(
        view.scheduled_at.unwrap().to_rfc3339(),
        "2026-11-02T14:00:00+00:00"
    );
    assert_eq!(
        view.scheduled_local.as_deref(),
        Some("Mon 2026-11-02 09:00 EST")
    );
    assert_eq!(view.hashtags, vec!["bread", "baking"]);

    h.app.submit_for_review(id).await.unwrap();
    let review = h.app.review(id).await.unwrap();
    assert!(!review.can_approve, "nothing rendered yet");
    assert_eq!(review.issues, vec![ValidationIssue::NotRendered]);
    assert!(h.app.approve(id, 1).await.is_err());

    h.app.render_variant(variant).await.unwrap();
    let ran = h.drain().await;
    assert_eq!(ran, vec![Disposition::Succeeded]);
    let review = h.app.review(id).await.unwrap();
    assert!(review.can_approve, "{review:?}");
    assert!(review.issues.is_empty(), "{:?}", review.issues);
    assert!(review.warnings.is_empty());
    assert_eq!(
        review.full_caption,
        "Crispy edges every time\n\n#bread #baking"
    );
    assert_eq!(review.account.display_name, "Bakery");
    assert!(Path::new(review.video_path.as_deref().unwrap()).is_file());

    let view = h.app.approve(id, 1).await.unwrap();
    assert_eq!(view.state, PublicationState::Scheduled);
    assert!(view.approved);
    assert!(view.actions.contains(&Action::PublishNow));
    let jobs = h.app.list_jobs(10).await.unwrap();
    let publish = jobs
        .iter()
        .find(|j| j.kind == "publish_publication")
        .unwrap();
    assert_eq!(publish.state, "queued");
    assert_eq!(publish.run_at.to_rfc3339(), "2026-11-02T14:00:00+00:00");

    // Not due yet.
    assert!(h.drain().await.is_empty());
    h.clock.set(view.scheduled_at.unwrap());
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(
        h.media.renders.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "render reused"
    );

    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::NeedsUserAction);
    assert!(view.actions.contains(&Action::ConfirmPosted));
    let dir = Path::new(view.package_dir.as_deref().unwrap());
    for file in [
        "video.mp4",
        "caption.txt",
        "hashtags.txt",
        "POSTING-STEPS.md",
        "manifest.json",
    ] {
        assert!(dir.join(file).is_file(), "missing {file}");
    }
    assert!(dir.starts_with(&h.app.paths().exports));

    let view = h
        .app
        .confirm_posted(id, Some(" https://www.instagram.com/reel/abc ".into()))
        .await
        .unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert_eq!(
        view.published_url.as_deref(),
        Some("https://www.instagram.com/reel/abc")
    );
    let attempts = h.app.store().attempts().for_publication(id).await.unwrap();
    assert!(matches!(
        attempts[0].outcome,
        AttemptOutcome::HandedOff { .. }
    ));
    let log: Vec<_> = h
        .app
        .store()
        .publications()
        .log(id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(
        log,
        vec![
            "created",
            "submit_for_review",
            "approve",
            "begin_preparing",
            "handoff_ready",
            "confirm_manual_post"
        ]
    );
}

async fn approved(
    h: &Harness,
    name: &str,
    platform: Platform,
    when: ScheduleInput,
) -> PublicationId {
    let (asset, variant) = imported(h, name).await;
    grant_permissions(h, asset).await;
    let account = h.app.add_manual_account(platform, "Studio").await.unwrap();
    let id = draft(h, asset, variant, &account.id, when).await;
    h.app.submit_for_review(id).await.unwrap();
    h.app.render_variant(variant).await.unwrap();
    h.drain().await;
    h.app.approve(id, 1).await.unwrap();
    id
}

#[tokio::test]
async fn editing_an_approved_publication_drops_approval_and_its_job() {
    let h = harness().await;
    let id = approved(
        &h,
        "a.mp4",
        Platform::YoutubeShorts,
        local("2026-11-02T09:00"),
    )
    .await;
    let view = h
        .app
        .edit_publication(
            id,
            EditPublication {
                caption: Some("A better caption".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(view.state, PublicationState::AwaitingApproval);
    assert_eq!(view.revision, 2);
    assert!(!view.approved);
    assert_eq!(view.approved_revision, None);
    let jobs = h.app.list_jobs(10).await.unwrap();
    assert!(
        jobs.iter()
            .filter(|j| j.kind == "publish_publication")
            .all(|j| j.state == "cancelled")
    );
    // Approving the old revision is refused; the current one works and gets a fresh job.
    assert!(h.app.approve(id, 1).await.is_err());
    h.app.approve(id, 2).await.unwrap();
    let open: Vec<_> = h
        .app
        .list_jobs(10)
        .await
        .unwrap()
        .into_iter()
        .filter(|j| j.kind == "publish_publication" && j.state == "queued")
        .collect();
    assert_eq!(open.len(), 1);
    let revisions = h.app.store().publications().revisions(id).await.unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].content.caption, "Crispy edges every time");
}

#[tokio::test]
async fn editing_a_variant_revises_publications_that_use_it() {
    let h = harness().await;
    let id = approved(
        &h,
        "b.mp4",
        Platform::YoutubeShorts,
        local("2026-11-02T09:00"),
    )
    .await;
    let view = h.app.publication(id).await.unwrap();
    let variant: VariantId = view.variant_id.parse().unwrap();
    let muted = RenderRecipe {
        audio: AudioChoice::Mute,
        ..Default::default()
    };
    let v = h.app.update_variant(variant, "Muted", muted).await.unwrap();
    assert_eq!(v.revision, 2);
    assert!(v.rendered.is_none(), "old render no longer applies");
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::AwaitingApproval);
    assert_eq!(view.revision, 2);
    let review = h.app.review(id).await.unwrap();
    assert!(!review.can_approve);
}

#[tokio::test]
async fn overdue_publications_expire_and_ask_by_default() {
    let h = harness().await;
    assert_eq!(
        h.app.preferences().await.unwrap().overdue_policy,
        OverduePolicy::Expire { grace_secs: 900 }
    );
    let id = approved(&h, "c.mp4", Platform::Linkedin, local("2026-11-02T09:00")).await;
    // The app was closed through the posting time.
    let due = h.app.publication(id).await.unwrap().scheduled_at.unwrap();
    h.clock.set(due + Duration::hours(3));
    let ran = h.drain().await;
    assert_eq!(ran, vec![Disposition::Expired { due }]);
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::NeedsUserAction);
    assert!(
        view.status_text.contains("passed while the app was closed"),
        "{}",
        view.status_text
    );
    assert_eq!(
        view.actions,
        vec![
            Action::Reschedule,
            Action::PublishNow,
            Action::Edit,
            Action::Cancel
        ]
    );

    // "Post now" keeps the approval and goes out on the next run.
    let view = h.app.schedule(id, ScheduleInput::Now).await.unwrap();
    assert_eq!(view.state, PublicationState::Scheduled);
    assert!(view.approved);
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::NeedsUserAction
    );
}

#[tokio::test]
async fn catch_up_policy_runs_overdue_work() {
    let h = harness().await;
    h.app
        .update_preferences(PreferencesUpdate {
            overdue_policy: Some(OverduePolicy::CatchUp),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = approved(&h, "d.mp4", Platform::Linkedin, local("2026-11-02T09:00")).await;
    h.clock.set(t0() + Duration::days(10));
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    let view = h.app.publication(id).await.unwrap();
    assert!(view.package_dir.is_some());
}

#[tokio::test]
async fn cancelling_a_scheduled_publication_cancels_its_job() {
    let h = harness().await;
    let id = approved(&h, "e.mp4", Platform::SnapchatSpotlight, ScheduleInput::Now).await;
    h.app.cancel_publication(id).await.unwrap();
    assert!(h.drain().await.is_empty());
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::Cancelled
    );
}

#[tokio::test]
async fn on_demand_export_writes_a_package_without_changing_state() {
    let h = harness().await;
    let (asset, variant) = imported(&h, "f.mp4").await;
    let account = h
        .app
        .add_manual_account(Platform::YoutubeShorts, "Channel")
        .await
        .unwrap();
    let id = draft(&h, asset, variant, &account.id, local("2026-11-02T09:00")).await;
    h.app.export_package(id).await.unwrap();
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    let jobs = h.app.list_jobs(10).await.unwrap();
    let export = jobs.iter().find(|j| j.kind == "export_package").unwrap();
    let dir = Path::new(export.package_dir.as_deref().unwrap());
    assert!(dir.join("title.txt").is_file());
    let steps = std::fs::read_to_string(dir.join("POSTING-STEPS.md")).unwrap();
    // Permissions were never recorded for this asset.
    assert!(steps.contains("Audio permission is not recorded as granted"));
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::Draft
    );
}

#[tokio::test]
async fn next_slot_skips_taken_times() {
    let h = harness().await;
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
    let (asset, variant) = imported(&h, "g.mp4").await;
    let account = h
        .app
        .add_manual_account(Platform::InstagramReels, "Reels")
        .await
        .unwrap();
    let account_id: AccountId = account.id.parse().unwrap();
    // Friday 30 October 2026 12:00Z. Next slot: Monday 2 November 18:00 EST = 23:00Z.
    let first = h.app.next_slot(account_id).await.unwrap().unwrap();
    assert_eq!(first.to_rfc3339(), "2026-11-02T23:00:00+00:00");
    draft(&h, asset, variant, &account.id, ScheduleInput::NextSlot).await;
    let second = h.app.next_slot(account_id).await.unwrap().unwrap();
    assert_eq!(second.to_rfc3339(), "2026-11-04T23:00:00+00:00");
}

#[tokio::test]
async fn monitor_lists_publications_and_jobs() {
    let h = harness().await;
    let id = approved(
        &h,
        "h.mp4",
        Platform::YoutubeShorts,
        local("2026-11-02T09:00"),
    )
    .await;
    let monitor = h.app.monitor().await.unwrap();
    assert_eq!(monitor.publications.len(), 1);
    assert_eq!(monitor.publications[0].id, id.to_string());
    assert!(
        monitor.publications[0]
            .status_text
            .starts_with("Approved; goes out")
    );
    assert!(
        monitor
            .jobs
            .iter()
            .any(|j| j.kind == "render_variant" && j.state == "succeeded")
    );
}

// --- automatic publishing through the adapter contract (fake adapter, no network) ----------

async fn automatic(adapter: Arc<FakeAdapter>) -> (Harness, PublicationId) {
    let mut registry = AdapterRegistry::new();
    registry.register(adapter.clone());
    let h = harness_with(registry).await;
    let (asset, variant) = imported(&h, "auto.mp4").await;
    grant_permissions(&h, asset).await;
    let mut account = ConnectedAccount::manual(adapter.platform, "API account", t0());
    account.capabilities = Capabilities {
        publish_access: PublishAccess::Automatic,
        ..Capabilities::manual_only(t0())
    };
    h.app.store().accounts().insert(&account).await.unwrap();
    let id = draft(
        &h,
        asset,
        variant,
        &account.id.to_string(),
        ScheduleInput::Now,
    )
    .await;
    h.app.submit_for_review(id).await.unwrap();
    h.app.render_variant(variant).await.unwrap();
    h.drain().await;
    h.app.approve(id, 1).await.unwrap();
    (h, id)
}

#[tokio::test]
async fn automatic_publish_goes_through_upload_processing_and_publish() {
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 3));
    let (h, id) = automatic(fake.clone()).await;
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert!(
        view.published_url
            .unwrap()
            .starts_with("https://example.invalid/post-")
    );
    assert_eq!(fake.count(Call::BeginUpload), 1);
    assert_eq!(fake.count(Call::UploadStep), 3);
    assert_eq!(fake.count(Call::Publish), 1);
    let events: Vec<_> = h
        .app
        .store()
        .publications()
        .log(id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.to)
        .collect();
    assert!(events.ends_with(&[
        PublicationState::Preparing,
        PublicationState::Uploading,
        PublicationState::Processing,
        PublicationState::Publishing,
        PublicationState::Published,
    ]));
    let attempt = &h.app.store().attempts().for_publication(id).await.unwrap()[0];
    assert!(matches!(attempt.outcome, AttemptOutcome::Published { .. }));
    assert!(attempt.publish_requested_at.is_some());
}

#[tokio::test]
async fn transient_upload_failure_resumes_from_the_checkpoint() {
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 3));
    let (h, id) = automatic(fake.clone()).await;
    // Fail the first chunk once.
    fake.fail_next(
        Call::UploadStep,
        AdapterError::Transient {
            message: "connection reset".into(),
            retry_after: None,
        },
    );
    let ran = h.drain().await;
    assert!(matches!(ran[..], [Disposition::RetryAt { .. }]), "{ran:?}");
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::RetryScheduled);
    assert_eq!(view.last_error.as_deref(), Some("connection reset"));

    h.clock.set(view.retry_at.unwrap());
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::Published
    );
    assert_eq!(
        fake.count(Call::BeginUpload),
        1,
        "the upload session was resumed"
    );
}

#[tokio::test]
async fn rate_limited_publish_waits_without_spending_retries() {
    let fake = Arc::new(FakeAdapter::new(Platform::InstagramReels, 1));
    let (h, id) = automatic(fake.clone()).await;
    fake.fail_next(
        Call::Publish,
        AdapterError::RateLimited {
            message: "429".into(),
            retry_after: std::time::Duration::from_secs(600),
        },
    );
    let ran = h.drain().await;
    let [
        Disposition::RetryAt {
            at,
            rate_limited: true,
            ..
        },
    ] = ran[..]
    else {
        panic!("{ran:?}")
    };
    assert!(at - h.clock_now() >= Duration::seconds(600));
    h.clock.set(at);
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::Published
    );
}

trait ClockNow {
    fn clock_now(&self) -> chrono::DateTime<chrono::Utc>;
}

impl ClockNow for Harness {
    fn clock_now(&self) -> chrono::DateTime<chrono::Utc> {
        use shorts_scheduler::Clock;
        self.clock.now()
    }
}

#[tokio::test]
async fn lost_publish_response_is_reconciled_when_possible() {
    let fake = Arc::new(FakeAdapter::new(Platform::Linkedin, 1));
    *fake.lose_publish_response.lock().unwrap() = true;
    let (h, id) = automatic(fake.clone()).await;
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::Published
    );
    assert_eq!(fake.count(Call::Publish), 1);
    assert_eq!(fake.count(Call::Reconcile), 1);
}

#[tokio::test]
async fn unreconcilable_publish_becomes_outcome_unknown_and_waits_for_the_user() {
    let fake = Arc::new(FakeAdapter::new(Platform::Linkedin, 1));
    *fake.lose_publish_response.lock().unwrap() = true;
    fake.fail_next(
        Call::Reconcile,
        AdapterError::Transient {
            message: "lookup failed".into(),
            retry_after: None,
        },
    );
    let (h, id) = automatic(fake.clone()).await;
    let ran = h.drain().await;
    assert!(matches!(ran[..], [Disposition::Paused { .. }]), "{ran:?}");
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::OutcomeUnknown);
    assert_eq!(view.actions, vec![Action::ResolveOutcome]);
    let attempt = &h.app.store().attempts().for_publication(id).await.unwrap()[0];
    assert!(matches!(attempt.outcome, AttemptOutcome::Unknown { .. }));

    // Nothing happens on its own, however long we wait.
    h.clock.set(t0() + Duration::days(30));
    assert!(h.drain().await.is_empty());
    assert_eq!(fake.count(Call::Publish), 1);

    // The user checked LinkedIn: it isn't there. Publishing runs again.
    let view = h
        .app
        .resolve_outcome(id, OutcomeResolution::NotPublished)
        .await
        .unwrap();
    assert_eq!(view.state, PublicationState::RetryScheduled);
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(fake.count(Call::Publish), 2);
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::Published
    );
}

#[tokio::test]
async fn user_can_confirm_an_unknown_outcome_was_published() {
    let fake = Arc::new(FakeAdapter::new(Platform::Linkedin, 1));
    *fake.lose_publish_response.lock().unwrap() = true;
    fake.fail_next(
        Call::Reconcile,
        AdapterError::Permanent {
            message: "no lookup API".into(),
        },
    );
    let (h, id) = automatic(fake.clone()).await;
    h.drain().await;
    let view = h
        .app
        .resolve_outcome(
            id,
            OutcomeResolution::Published {
                url: Some("https://www.linkedin.com/feed/update/1".into()),
                remote_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert!(h.drain().await.is_empty());
    let jobs = h.app.list_jobs(10).await.unwrap();
    assert!(
        jobs.iter()
            .filter(|j| j.kind == "publish_publication")
            .all(|j| j.state == "cancelled")
    );
    assert_eq!(fake.count(Call::Publish), 1);
}

#[tokio::test]
async fn invalid_submissions_pause_for_correction() {
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 1));
    fake.fail_next(
        Call::Validate,
        AdapterError::Invalid {
            issues: vec!["title is required".into()],
        },
    );
    let (h, id) = automatic(fake.clone()).await;
    let ran = h.drain().await;
    assert!(matches!(ran[..], [Disposition::Paused { .. }]));
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::NeedsUserAction);
    assert!(view.status_text.contains("title is required"));
    // After fixing things outside the app, the user resumes it.
    h.app.resolve_user_action(id).await.unwrap();
    assert_eq!(h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(
        h.app.publication(id).await.unwrap().state,
        PublicationState::Published
    );
}

#[tokio::test]
async fn auth_failures_pause_as_needs_authentication() {
    let fake = Arc::new(FakeAdapter::new(Platform::InstagramReels, 2));
    fake.fail_next(
        Call::BeginUpload,
        AdapterError::Authentication {
            message: "token expired".into(),
        },
    );
    let (h, id) = automatic(fake).await;
    h.drain().await;
    let view = h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::NeedsAuthentication);
    assert!(view.status_text.contains("Reconnect API account"));
}
