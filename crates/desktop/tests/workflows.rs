//! Workflows end to end: a file dropped into the watch folder reaches Published on a fake
//! YouTube adapter with no manual calls when Auto Publish is on, and waits for review when it
//! is off. Also covers copy fitting, rights, the TikTok caption wait and workflow editing.

mod support;

use std::sync::Arc;
use std::time::Duration;

use chrono::Duration as ChronoDuration;
use serde_json::json;
use shorts_adapters::AdapterRegistry;
use shorts_adapters::fake::{Call, FakeAdapter};
use shorts_desktop::watch::{WatchConfig, WatchFolder};
use shorts_desktop::{CAPTION_WAIT_MINUTES, NewWorkflow, WorkflowUpdate};
use shorts_domain::workflow::WorkflowStatus;
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, PermissionStatus, Platform, PostingMode,
    Publication, PublicationState, PublishAccess, WorkflowId, WorkflowSettings,
};
use shorts_store::PublicationFilter;
use support::{Harness, harness_with, t0, worker_config};
use tokio_util::sync::CancellationToken;

fn fast() -> WatchConfig {
    WatchConfig {
        settle: Duration::from_millis(200),
        poll: Duration::from_millis(50),
        rescan: Duration::from_secs(3600),
    }
}

async fn automatic_account(h: &Harness, platform: Platform, name: &str) -> AccountId {
    let mut account = ConnectedAccount::manual(platform, name, t0());
    account.capabilities = Capabilities {
        publish_access: PublishAccess::Automatic,
        ..Capabilities::manual_only(t0())
    };
    h.app.store().accounts().insert(&account).await.unwrap();
    account.id
}

struct Setup {
    h: Harness,
    youtube: Arc<FakeAdapter>,
    account: AccountId,
}

async fn setup() -> Setup {
    let youtube = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 2));
    let mut registry = AdapterRegistry::new();
    registry.register(youtube.clone());
    let h = harness_with(registry).await;
    let account = automatic_account(&h, Platform::YoutubeShorts, "My channel").await;
    Setup {
        h,
        youtube,
        account,
    }
}

fn new_workflow(account: AccountId, auto_publish: bool) -> NewWorkflow {
    NewWorkflow {
        account_id: account,
        name: None,
        auto_publish,
        rights_confirmed: true,
        effective_from: None,
        settings: WorkflowSettings::default(),
        caption_template: None,
        hashtags: vec![],
    }
}

async fn publications(h: &Harness) -> Vec<Publication> {
    h.app
        .store()
        .publications()
        .list(&PublicationFilter::default())
        .await
        .unwrap()
}

/// Polls until a publication reaches `state` (or fails the test after 15 s).
async fn wait_for(h: &Harness, state: PublicationState) -> Publication {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(p) = publications(h)
                .await
                .into_iter()
                .find(|p| p.state() == state)
            {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("no publication reached {state:?}");
    })
}

/// Starts the watch folder and the worker, as the desktop shell does.
async fn run(h: &Harness) -> (std::path::PathBuf, WatchFolder, CancellationToken) {
    let folder = h.dir.path().join("phone-exports");
    std::fs::create_dir_all(&folder).unwrap();
    let watch = WatchFolder::start(h.app.clone(), folder.clone(), fast()).unwrap();
    let shutdown = CancellationToken::new();
    h.app
        .start_worker(worker_config(), shutdown.clone())
        .await
        .unwrap();
    (folder, watch, shutdown)
}

#[tokio::test]
async fn a_dropped_file_is_published_with_no_clicks_under_auto_publish() {
    let s = setup().await;
    let w =
        s.h.app
            .create_workflow(new_workflow(s.account, true))
            .await
            .unwrap();
    assert_eq!(w.name, "TikTok → YouTube Shorts");
    assert!(w.auto_publish);
    assert_eq!(w.status, WorkflowStatus::Active);
    assert!(
        w.banner.contains("automatically within ~5 minutes"),
        "{}",
        w.banner
    );
    let (folder, _watch, shutdown) = run(&s.h).await;

    // The editor exported the video with a caption file next to it.
    std::fs::write(
        folder.join("sourdough-day.json"),
        json!({"description": "Sourdough day <3", "hashtags": ["bread", "#baking"]}).to_string(),
    )
    .unwrap();
    std::fs::write(folder.join("sourdough-day.mp4"), b"a vertical clip").unwrap();

    let p = wait_for(&s.h, PublicationState::Published).await;
    shutdown.cancel();
    assert_eq!(s.youtube.count(Call::Publish), 1);
    let approval = p.approval().unwrap();
    assert_eq!(approval.approved_by, "workflow:TikTok → YouTube Shorts");
    assert_eq!(approval.revision, 1, "the revision the workflow created");
    // Copy came from the sidecar, fitted for YouTube (no "<").
    assert_eq!(p.content().caption, "Sourdough day 3");
    assert_eq!(p.content().hashtags, vec!["bread", "baking"]);
    // The rights checkbox recorded both permissions.
    let asset =
        s.h.app
            .store()
            .assets()
            .require(p.asset_id())
            .await
            .unwrap();
    assert_eq!(asset.video_permission.status, PermissionStatus::Granted);
    assert_eq!(asset.audio_permission.status, PermissionStatus::Granted);
    assert!(
        asset
            .audio_permission
            .note
            .unwrap()
            .contains("TikTok → YouTube Shorts")
    );

    let view = s.h.app.publication(p.id()).await.unwrap();
    assert_eq!(view.status_label, "Published");
    assert_eq!(
        view.approved_by.as_deref(),
        Some("workflow:TikTok → YouTube Shorts")
    );
    let detail =
        s.h.app
            .workflow_detail(w.id.parse().unwrap())
            .await
            .unwrap();
    assert_eq!(detail.workflow.item_count, 1);
    assert_eq!(detail.items.len(), 1);
    assert_eq!(detail.items[0].publications[0].status_label, "Published");
    let activity = s.h.app.activity(50).await.unwrap();
    assert_eq!(activity.items.len(), 1);
}

#[tokio::test]
async fn a_private_youtube_upload_is_not_called_published() {
    let s = setup().await;
    *s.youtube.publish_note.lock().unwrap() = Some("Uploaded as private.".into());
    s.h.app
        .create_workflow(new_workflow(s.account, true))
        .await
        .unwrap();
    let path = s.h.video("private.mp4", b"private clip");
    s.h.app.import_watched(&path).await.unwrap();
    s.h.drain().await;
    let p = publications(&s.h).await.remove(0);
    assert_eq!(p.state(), PublicationState::Published);
    let view = s.h.app.publication(p.id()).await.unwrap();
    assert_eq!(view.status_label, "Uploaded (private)");
    assert!(view.published_private);
    assert!(
        view.status_text.contains("YouTube Studio"),
        "{}",
        view.status_text
    );

    // Republish posts it again as a new publication approved by the owner.
    let again = s.h.app.republish(p.id()).await.unwrap();
    assert_ne!(again.id, p.id().to_string());
    assert_eq!(again.approved_by.as_deref(), Some("owner"));
    s.h.drain().await;
    assert_eq!(s.youtube.count(Call::Publish), 2);
}

#[tokio::test]
async fn with_review_required_it_waits_until_the_owner_publishes() {
    let s = setup().await;
    let w =
        s.h.app
            .create_workflow(new_workflow(s.account, false))
            .await
            .unwrap();
    assert!(!w.auto_publish);
    assert!(w.banner.ends_with("waits for your review."), "{}", w.banner);
    let (folder, _watch, shutdown) = run(&s.h).await;
    std::fs::write(folder.join("clip.mp4"), b"another clip").unwrap();

    let p = wait_for(&s.h, PublicationState::AwaitingApproval).await;
    // Rendered automatically so review is instant; nothing goes out by itself.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let view = s.h.app.publication(p.id()).await.unwrap();
            if view.status_label == "Waiting for review" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the video was never rendered");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.youtube.count(Call::BeginUpload), 0);
    let p =
        s.h.app
            .store()
            .publications()
            .require(p.id())
            .await
            .unwrap();
    assert_eq!(p.state(), PublicationState::AwaitingApproval);
    assert_eq!(p.content().caption, "clip", "a title from the file name");

    let view = s.h.app.publish_now(p.id()).await.unwrap();
    assert_eq!(view.approved_by.as_deref(), Some("owner"));
    wait_for(&s.h, PublicationState::Published).await;
    shutdown.cancel();
}

#[tokio::test]
async fn turning_auto_publish_on_approves_what_was_waiting() {
    let s = setup().await;
    let w =
        s.h.app
            .create_workflow(new_workflow(s.account, false))
            .await
            .unwrap();
    let id: WorkflowId = w.id.parse().unwrap();
    let path = s.h.video("waiting.mp4", b"waiting clip");
    s.h.app.import_watched(&path).await.unwrap();
    s.h.drain().await;
    let p = publications(&s.h).await.remove(0);
    assert_eq!(p.state(), PublicationState::AwaitingApproval);

    let view =
        s.h.app
            .update_workflow(
                id,
                WorkflowUpdate {
                    auto_publish: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    assert!(view.auto_publish);
    let p =
        s.h.app
            .store()
            .publications()
            .require(p.id())
            .await
            .unwrap();
    assert_eq!(p.state(), PublicationState::Scheduled);
    assert_eq!(
        p.approval().unwrap().approved_by,
        "workflow:TikTok → YouTube Shorts"
    );
}

#[tokio::test]
async fn copy_is_fitted_to_each_destination() {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(FakeAdapter::new(Platform::SnapchatSpotlight, 1)));
    let h = harness_with(registry).await;
    let snap = automatic_account(&h, Platform::SnapchatSpotlight, "Snap").await;
    h.app
        .create_workflow(NewWorkflow {
            hashtags: vec!["extra".into()],
            ..new_workflow(snap, true)
        })
        .await
        .unwrap();
    let path = h.video("long.mp4", b"long caption clip");
    let caption = format!("{} #one #two", "word ".repeat(40).trim());
    std::fs::write(path.with_extension("txt"), &caption).unwrap();
    h.app.import_watched(&path).await.unwrap();
    h.drain().await;

    let p = publications(&h).await.remove(0);
    let full = shorts_domain::platform::full_caption(&p.content().caption, &p.content().hashtags);
    assert!(full.chars().count() <= 160, "{full}");
    assert!(p.content().caption.ends_with('…'));
    // Fitted copy has nothing for review to flag, so the workflow approved and posted it.
    assert_eq!(p.state(), PublicationState::Published);
    assert!(p.approval().is_some());
}

#[tokio::test]
async fn a_denied_permission_is_not_overridden_by_the_rights_checkbox() {
    let s = setup().await;
    s.h.app
        .create_workflow(new_workflow(s.account, true))
        .await
        .unwrap();
    let path = s.h.video("denied.mp4", b"someone else's clip");
    let asset = s.h.app.import_file(&path, None).await.unwrap().asset_id();
    s.h.app
        .update_asset(
            asset,
            shorts_desktop::AssetUpdate {
                video_permission: Some(shorts_desktop::PermissionInput {
                    status: PermissionStatus::Denied,
                    note: None,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let c = s.h.app.source_collection().await.unwrap();
    s.h.app.add_to_collection(c, asset).await.unwrap();
    s.h.drain().await;
    let p = publications(&s.h).await.remove(0);
    assert_eq!(p.state(), PublicationState::AwaitingApproval);
}

#[tokio::test]
async fn with_tiktok_connected_a_file_waits_briefly_for_its_caption() {
    let s = setup().await;
    let w =
        s.h.app
            .create_workflow(new_workflow(s.account, true))
            .await
            .unwrap();
    // As if TikTok new-post detection were connected.
    let settings = s.h.app.store().settings();
    settings.set("tiktok_enabled", &true).await.unwrap();
    settings
        .set(
            "tiktok_source",
            &json!({
                "open_id": "o", "display_name": "me", "connected_at": t0(), "since": t0(),
                "last_checked_at": null, "last_error": null, "needs_reconnect": false
            }),
        )
        .await
        .unwrap();

    let path = s.h.video("no-caption.mp4", b"no caption clip");
    s.h.app.import_watched(&path).await.unwrap();
    assert!(publications(&s.h).await.is_empty());
    let detail =
        s.h.app
            .workflow_detail(w.id.parse().unwrap())
            .await
            .unwrap();
    assert!(
        detail.items[0]
            .waiting_reason
            .as_deref()
            .unwrap()
            .contains("TikTok post")
    );

    // A file with its own caption doesn't wait.
    let other = s.h.video("captioned.mp4", b"captioned clip");
    std::fs::write(other.with_extension("txt"), "Has a caption").unwrap();
    s.h.app.import_watched(&other).await.unwrap();
    assert_eq!(publications(&s.h).await.len(), 1);

    // Once the wait is over, the next rescan publishes it with the file name's title.
    s.h.clock
        .advance(ChronoDuration::minutes(CAPTION_WAIT_MINUTES + 1));
    s.h.app.apply_deferred_rules().await;
    let all = publications(&s.h).await;
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|p| p.content().caption == "no caption"));
}

#[tokio::test]
async fn workflows_can_be_paused_reactivated_and_removed() {
    let s = setup().await;
    let manual =
        s.h.app
            .add_manual_account(Platform::Linkedin, "Page")
            .await
            .unwrap();
    // Suggestions only offer accounts that can post automatically.
    let list = s.h.app.list_workflows().await.unwrap();
    assert_eq!(list.suggestions.len(), 1);
    assert_eq!(list.suggestions[0].name, "TikTok → YouTube Shorts");

    let err =
        s.h.app
            .create_workflow(new_workflow(manual.id.parse().unwrap(), true))
            .await
            .unwrap_err();
    assert!(err.to_string().contains("Connect Page first"), "{err}");

    let w =
        s.h.app
            .create_workflow(new_workflow(s.account, true))
            .await
            .unwrap();
    let id: WorkflowId = w.id.parse().unwrap();
    let again =
        s.h.app
            .create_workflow(new_workflow(s.account, true))
            .await
            .unwrap_err();
    assert!(again.to_string().contains("already a workflow"), "{again}");
    assert!(
        s.h.app
            .list_workflows()
            .await
            .unwrap()
            .suggestions
            .is_empty()
    );

    let paused =
        s.h.app
            .update_workflow(
                id,
                WorkflowUpdate {
                    enabled: Some(false),
                    name: Some("Shorts".into()),
                    posting: Some(PostingMode::Slots),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    assert_eq!(paused.status, WorkflowStatus::Inactive);
    assert_eq!(paused.name, "Shorts");
    assert_eq!(paused.posting, PostingMode::Slots);
    let path = s.h.video("while-paused.mp4", b"paused clip");
    s.h.app.import_watched(&path).await.unwrap();
    assert!(publications(&s.h).await.is_empty());

    // Reactivating starts from now: what arrived while paused isn't published.
    s.h.clock.advance(ChronoDuration::minutes(5));
    let active =
        s.h.app
            .update_workflow(
                id,
                WorkflowUpdate {
                    enabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    assert_eq!(active.status, WorkflowStatus::Active);
    assert_eq!(active.effective_from, t0() + ChronoDuration::minutes(5));
    s.h.app.apply_deferred_rules().await;
    assert!(publications(&s.h).await.is_empty());

    s.h.app.delete_workflow(id).await.unwrap();
    assert!(s.h.app.list_workflows().await.unwrap().workflows.is_empty());

    // Deleting a workflow's rule from Collections removes the workflow and its grant too.
    let w =
        s.h.app
            .create_workflow(new_workflow(s.account, true))
            .await
            .unwrap();
    let c = s.h.app.source_collection().await.unwrap();
    s.h.app
        .delete_rule(w.rule_id.parse().unwrap())
        .await
        .unwrap();
    assert!(s.h.app.list_workflows().await.unwrap().workflows.is_empty());
    assert!(
        s.h.app
            .store()
            .collections()
            .grant(c, s.account)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_file_that_matches_one_tiktok_post_takes_its_caption() {
    let s = setup().await;
    s.h.app
        .create_workflow(new_workflow(s.account, true))
        .await
        .unwrap();
    // A detected post waiting for its original (the fake probe reports 20 s).
    let mut item = shorts_domain::Asset::awaiting_media(
        "Bagels",
        Some("https://www.tiktok.com/@me/video/1"),
        t0(),
    )
    .unwrap();
    item.notes = "Bagel morning #bagels #breakfast".into();
    let post = shorts_domain::TikTokPost {
        video_id: "1".into(),
        asset_id: item.id,
        posted_at: t0(),
        duration_ms: Some(20_000),
        title: "Bagels".into(),
        caption: item.notes.clone(),
        share_url: item.source_url.clone(),
        cover_url: None,
        cover_path: None,
        discovered_at: t0(),
        matched_at: None,
    };
    assert!(
        s.h.app
            .store()
            .record_tiktok_post(&item, &post)
            .await
            .unwrap()
    );

    let path = s.h.video("export-0412.mp4", b"bagel clip");
    std::fs::write(path.with_extension("txt"), "Sidecar caption").unwrap();
    s.h.app.import_watched(&path).await.unwrap();
    s.h.drain().await;
    let p = publications(&s.h).await.remove(0);
    assert_eq!(p.content().caption, "Bagel morning");
    assert_eq!(p.content().hashtags, vec!["bagels", "breakfast"]);
    assert_eq!(p.state(), PublicationState::Published);
}
