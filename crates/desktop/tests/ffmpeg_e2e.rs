//! The manual-handoff workflow with real FFmpeg. Skips when FFmpeg is not installed.

use std::path::Path;
use std::sync::Arc;

use shorts_adapters::AdapterRegistry;
use shorts_desktop::{App, AppPaths, FfmpegBackend, ImportOutcome, NewPublication, ScheduleInput};
use shorts_domain::{Platform, PublicationState};
use shorts_media::testing::{sample_video, tools_or_skip};
use shorts_media::{RenderCache, Renderer};
use shorts_scheduler::{SystemClock, WorkerConfig};

#[tokio::test]
async fn import_render_and_export_with_ffmpeg() {
    let Some(tools) = tools_or_skip("import_render_and_export_with_ffmpeg") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::under(&dir.path().join("data"));
    let backend = FfmpegBackend::new(Renderer::new(
        tools.clone(),
        RenderCache::new(paths.cache.clone()),
        1,
    ));
    let app = App::open(
        paths,
        Arc::new(backend),
        AdapterRegistry::new(),
        Arc::new(SystemClock),
    )
    .await
    .unwrap();

    let source = dir.path().join("landscape clip.mp4");
    sample_video(&tools, &source, 3, 640, 360, true).await;
    let ImportOutcome::Imported { asset_id } = app.import_file(&source, None).await.unwrap() else {
        panic!()
    };
    let detail = app.asset_detail(asset_id).await.unwrap();
    let media = detail.asset.media.as_ref().unwrap();
    assert_eq!(
        (media.properties.width, media.properties.height),
        (640, 360)
    );
    assert!(app.thumbnail(asset_id).await.unwrap().unwrap().is_file());

    let account = app
        .add_manual_account(Platform::YoutubeShorts, "Channel")
        .await
        .unwrap();
    let variant = detail.variants[0].id.parse().unwrap();
    let p = app
        .create_publication(NewPublication {
            asset_id,
            variant_id: variant,
            account_id: account.id.parse().unwrap(),
            caption: "Colour bars".into(),
            hashtags: vec!["test".into()],
            schedule: Some(ScheduleInput::Now),
        })
        .await
        .unwrap();
    let id = p.id.parse().unwrap();
    app.submit_for_review(id).await.unwrap();
    app.render_variant(variant).await.unwrap();
    let worker = app.worker(WorkerConfig::default()).await.unwrap();
    worker.run_once().await.unwrap().unwrap();

    let review = app.review(id).await.unwrap();
    let rendered = review.variant.rendered.as_ref().unwrap();
    assert_eq!(
        (rendered.properties.width, rendered.properties.height),
        (1080, 1920)
    );
    assert_eq!(rendered.properties.video_codec.as_deref(), Some("h264"));
    assert_eq!(rendered.properties.audio_codec.as_deref(), Some("aac"));
    assert!(review.can_approve);

    app.approve(id, 1).await.unwrap();
    worker.run_once().await.unwrap().unwrap();
    let view = app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::NeedsUserAction);
    let package = Path::new(view.package_dir.as_deref().unwrap());
    let probed = shorts_media::probe(&tools, &package.join("video.mp4"))
        .await
        .unwrap();
    assert_eq!((probed.width, probed.height), (1080, 1920));
}
