//! A crash while a publish request is in flight must not lead to a second post.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use shorts_adapters::AdapterRegistry;
use shorts_adapters::fake::{Call, FakeAdapter};
use shorts_desktop::{
    App, AppPaths, AssetUpdate, ImportOutcome, NewPublication, PermissionInput, ScheduleInput,
};
use shorts_domain::{
    Capabilities, ConnectedAccount, PermissionStatus, Platform, PublicationId, PublicationState,
    PublishAccess,
};
use shorts_scheduler::{Disposition, ManualClock};
use support::{FakeMedia, t0, worker_config};

#[test]
fn crash_during_publish_reconciles_instead_of_publishing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::under(&dir.path().join("data"));
    let media = Arc::new(FakeMedia {
        cache: paths.cache.clone(),
        ..Default::default()
    });
    let clock = ManualClock::new(t0());
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 2));
    let registry = || {
        let mut r = AdapterRegistry::new();
        r.register(fake.clone());
        r
    };

    // First process: get to the publish request, which reaches the provider, then die.
    let first = tokio::runtime::Runtime::new().unwrap();
    let (id, first_app): (PublicationId, _) = first.block_on(async {
        let app = App::open(
            paths.clone(),
            media.clone(),
            registry(),
            Arc::new(clock.clone()),
        )
        .await
        .unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"clip").unwrap();
        let ImportOutcome::Imported { asset_id } = app.import_file(&video, None).await.unwrap()
        else {
            panic!()
        };
        let granted = || {
            Some(PermissionInput {
                status: PermissionStatus::Granted,
                note: None,
            })
        };
        app.update_asset(
            asset_id,
            AssetUpdate {
                video_permission: granted(),
                audio_permission: granted(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let variant = app.asset_detail(asset_id).await.unwrap().variants[0]
            .id
            .parse()
            .unwrap();
        let mut account = ConnectedAccount::manual(Platform::YoutubeShorts, "Channel", t0());
        account.capabilities = Capabilities {
            publish_access: PublishAccess::Automatic,
            ..Capabilities::manual_only(t0())
        };
        app.store().accounts().insert(&account).await.unwrap();
        let p = app
            .create_publication(NewPublication {
                asset_id,
                variant_id: variant,
                account_id: account.id,
                caption: "Hello".into(),
                hashtags: vec![],
                schedule: Some(ScheduleInput::Now),
            })
            .await
            .unwrap();
        let id: PublicationId = p.id.parse().unwrap();
        app.submit_for_review(id).await.unwrap();
        app.render_variant(variant).await.unwrap();
        let worker = app.worker(worker_config()).await.unwrap();
        worker.run_once().await.unwrap();
        app.approve(id, 1).await.unwrap();
        *fake.hang_after_publish.lock().unwrap() = true;
        let worker = worker.clone();
        tokio::spawn(async move {
            let _ = worker.run_once().await;
        });
        (id, app)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while fake.published.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "publish never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    first.shutdown_background();
    // A crash runs no destructors.
    std::mem::forget(first_app);

    // Second process: recover and finish.
    let second = tokio::runtime::Runtime::new().unwrap();
    second.block_on(async {
        let app = App::open(
            paths.clone(),
            media.clone(),
            registry(),
            Arc::new(clock.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            app.publication(id).await.unwrap().state,
            PublicationState::Publishing,
            "left mid-publish by the crash"
        );
        let worker = app.worker(worker_config()).await.unwrap();
        assert_eq!(worker.recover().await.unwrap().len(), 1);
        let (_, d) = worker.run_once().await.unwrap().unwrap();
        assert_eq!(d, Disposition::Succeeded);
        let view = app.publication(id).await.unwrap();
        assert_eq!(view.state, PublicationState::Published);
        assert!(view.published_url.is_some());
    });
    assert_eq!(fake.count(Call::Publish), 1, "never published twice");
    assert_eq!(fake.count(Call::Reconcile), 1);
    assert_eq!(fake.published.lock().unwrap().len(), 1);
}
