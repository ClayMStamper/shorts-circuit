use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use shorts_adapters::fake::{Call, FakeAdapter};
use shorts_adapters::{
    AdapterError, AdapterRegistry, ErasedAdapter, ManualHandoff, PackageRequest, Processing,
    Reconciliation, Submission, UploadProgress,
};
use shorts_domain::{
    AssetId, ConnectedAccount, Destination, MediaProperties, Platform, Publication, PublishAttempt,
    RevisionContent, Schedule, TimeZoneName, ValidationIssue, VariantId,
};

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-02T15:30:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn publication(account: &ConnectedAccount) -> Publication {
    let content = RevisionContent::new(
        VariantId::new(),
        1,
        "Crispy edges every time\nFull recipe in bio",
        &["bread".into(), "#baking".into()],
        Destination {
            account_id: account.id,
            platform: account.platform,
        },
    )
    .unwrap();
    Publication::draft(
        AssetId::new(),
        content,
        Some(Schedule::new(
            t0(),
            TimeZoneName::parse("America/Los_Angeles").unwrap(),
        )),
        t0(),
    )
}

fn media(duration_ms: u64) -> MediaProperties {
    MediaProperties {
        duration_ms,
        width: 1080,
        height: 1920,
        audio_streams: 1,
        video_codec: Some("h264".into()),
        audio_codec: Some("aac".into()),
        ..Default::default()
    }
}

/// Stores a checkpoint the way the scheduler would: as JSON text in the database.
fn persist(value: &Value) -> Value {
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

#[tokio::test]
async fn erased_adapter_round_trips_typed_checkpoints() {
    let account = ConnectedAccount::manual(Platform::YoutubeShorts, "Test channel", t0());
    let p = publication(&account);
    let props = media(20_000);
    let sub = Submission {
        publication: &p,
        account: &account,
        media_path: Path::new("video.mp4"),
        media: &props,
        size_bytes: 1_000,
    };
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 3));
    let adapter: Arc<dyn ErasedAdapter> = fake.clone();

    assert!(
        adapter
            .capabilities(&account)
            .await
            .unwrap()
            .can_publish_automatically()
    );
    assert!(adapter.validate(&sub).await.unwrap().is_empty());
    let mut checkpoint = persist(&adapter.begin_upload(&sub).await.unwrap());
    let remote = loop {
        match adapter.upload_step(&sub, checkpoint).await.unwrap() {
            UploadProgress::InProgress { checkpoint: c, .. } => checkpoint = persist(&c),
            UploadProgress::Complete { media } => break persist(&media),
        }
    };
    assert_eq!(fake.count(Call::UploadStep), 3);
    while let Processing::Pending { .. } = adapter.check_processing(&remote).await.unwrap() {}
    let post = adapter.publish(&sub, &remote).await.unwrap();
    assert!(post.remote_id.starts_with("post-media-session-"));
    assert!(post.url.is_some());
}

#[tokio::test]
async fn incompatible_checkpoints_are_rejected_not_misread() {
    let account = ConnectedAccount::manual(Platform::YoutubeShorts, "Test channel", t0());
    let p = publication(&account);
    let props = media(20_000);
    let sub = Submission {
        publication: &p,
        account: &account,
        media_path: Path::new("video.mp4"),
        media: &props,
        size_bytes: 1_000,
    };
    let adapter: Arc<dyn ErasedAdapter> = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 1));
    let err = adapter
        .upload_step(&sub, serde_json::json!({"offset": 12}))
        .await
        .unwrap_err();
    assert!(matches!(err, AdapterError::Permanent { .. }));
}

#[tokio::test]
async fn lost_publish_responses_can_be_reconciled() {
    let account = ConnectedAccount::manual(Platform::InstagramReels, "Test", t0());
    let p = publication(&account);
    let props = media(20_000);
    let sub = Submission {
        publication: &p,
        account: &account,
        media_path: Path::new("video.mp4"),
        media: &props,
        size_bytes: 1_000,
    };
    let fake = Arc::new(FakeAdapter::new(Platform::InstagramReels, 1));
    *fake.lose_publish_response.lock().unwrap() = true;
    let adapter: Arc<dyn ErasedAdapter> = fake.clone();
    let checkpoint = adapter.begin_upload(&sub).await.unwrap();
    let UploadProgress::Complete { media } = adapter.upload_step(&sub, checkpoint).await.unwrap()
    else {
        panic!("one chunk")
    };
    let err = adapter.publish(&sub, &media).await.unwrap_err();
    assert!(matches!(err, AdapterError::ResponseLost { .. }));

    let attempt = PublishAttempt::start(p.id(), 1, None, t0());
    match adapter.reconcile(&attempt, Some(&media)).await.unwrap() {
        Reconciliation::Found { post } => assert!(post.remote_id.starts_with("post-")),
        other => panic!("expected the post to be found, got {other:?}"),
    }
    assert!(matches!(
        adapter.reconcile(&attempt, None).await.unwrap(),
        Reconciliation::Unavailable { .. }
    ));
}

#[test]
fn registry_falls_back_to_manual_when_empty() {
    let mut registry = AdapterRegistry::new();
    assert!(registry.get(Platform::Linkedin).is_none());
    registry.register(Arc::new(FakeAdapter::new(Platform::Linkedin, 1)));
    assert!(registry.get(Platform::Linkedin).is_some());
    assert!(registry.get(Platform::SnapchatSpotlight).is_none());
}

async fn export(
    platform: Platform,
    duration_ms: u64,
    warnings: &[String],
    root: &Path,
) -> (shorts_adapters::ExportedPackage, Publication) {
    let video = root.join("render.mp4");
    std::fs::write(&video, b"fake mp4 bytes").unwrap();
    let account = ConnectedAccount::manual(platform, "Bakery Studio", t0());
    let p = publication(&account);
    let props = media(duration_ms);
    let handoff = ManualHandoff::new(root.join("exports"));
    let package = handoff
        .export(&PackageRequest {
            publication: &p,
            account: &account,
            asset_title: "Sourdough: Day 3",
            video_path: &video,
            media: &props,
            size_bytes: 14,
            warnings,
            exported_at: t0(),
        })
        .await
        .unwrap();
    (package, p)
}

#[tokio::test]
async fn youtube_package_contains_video_copy_and_steps() {
    let dir = tempfile::tempdir().unwrap();
    let (package, p) = export(Platform::YoutubeShorts, 30_000, &[], dir.path()).await;

    let name = package
        .dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // The date is the local posting date in the user's timezone.
    assert!(
        name.starts_with("2026-11-02_youtube_shorts_sourdough-day-3_"),
        "{name}"
    );
    assert!(name.ends_with("_r1"));
    assert_eq!(std::fs::read(&package.video).unwrap(), b"fake mp4 bytes");

    let caption = std::fs::read_to_string(package.dir.join("caption.txt")).unwrap();
    assert_eq!(
        caption,
        "Crispy edges every time\nFull recipe in bio\n\n#bread #baking\n"
    );
    let title = std::fs::read_to_string(package.dir.join("title.txt")).unwrap();
    assert_eq!(title, "Crispy edges every time\n");
    let steps = std::fs::read_to_string(package.dir.join("POSTING-STEPS.md")).unwrap();
    assert!(steps.contains("# Post to YouTube Shorts"));
    assert!(steps.contains("Bakery Studio"));
    assert!(
        steps.contains(
            "**Monday 2 November 2026, 07:30** (America/Los_Angeles; 2026-11-02 15:30 UTC)"
        )
    );
    assert!(steps.contains("Mark as posted"));
    assert!(!steps.contains("Check first"));

    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(package.dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["publication_id"], p.id().to_string());
    assert_eq!(manifest["revision"], 1);
    assert_eq!(manifest["platform"], "youtube_shorts");
    assert_eq!(manifest["scheduled_for"], "2026-11-02T15:30:00Z");

    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("exports"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, vec![name]);
}

#[tokio::test]
async fn spotlight_package_flags_problems_and_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let warnings = vec!["Audio permission is not recorded as granted.".to_owned()];
    let (package, _) = export(Platform::SnapchatSpotlight, 90_000, &warnings, dir.path()).await;
    assert!(!package.dir.join("title.txt").exists());
    assert!(matches!(
        package.manifest.issues[..],
        [ValidationIssue::TooLong { .. }]
    ));
    let steps = std::fs::read_to_string(package.dir.join("POSTING-STEPS.md")).unwrap();
    assert!(steps.contains("## Check first"));
    assert!(steps.contains("the maximum is 60.0s"));
    assert!(steps.contains("Audio permission is not recorded as granted."));
    assert!(steps.contains("Send To → Spotlight"));
}

#[tokio::test]
async fn re_exporting_replaces_the_previous_package() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("render.mp4");
    std::fs::write(&video, b"v1").unwrap();
    let account = ConnectedAccount::manual(Platform::Linkedin, "Bakery Studio", t0());
    let p = publication(&account);
    let props = media(30_000);
    let handoff = ManualHandoff::new(dir.path().join("exports"));
    let request = PackageRequest {
        publication: &p,
        account: &account,
        asset_title: "Clip",
        video_path: &video,
        media: &props,
        size_bytes: 2,
        warnings: &[],
        exported_at: t0(),
    };
    let first = handoff.export(&request).await.unwrap();
    std::fs::write(first.dir.join("stale.txt"), b"old").unwrap();
    std::fs::write(&video, b"v2").unwrap();
    let second = handoff.export(&request).await.unwrap();
    assert_eq!(first.dir, second.dir);
    assert!(!second.dir.join("stale.txt").exists());
    assert_eq!(std::fs::read(&second.video).unwrap(), b"v2");
}
