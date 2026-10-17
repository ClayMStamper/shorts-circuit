//! End-to-end media tests. They need `ffmpeg` and `ffprobe` on PATH (or `SHORTS_FFMPEG` /
//! `SHORTS_FFPROBE`) and skip with a message otherwise.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use shorts_domain::{
    AudioChoice, CropRect, FitMode, MediaProperties, RenderRecipe, SubtitleMode, SubtitleTrack,
    TrimRange,
};
use shorts_media::testing::{sample_video, tools_or_skip};
use shorts_media::{
    MediaTools, RenderCache, RenderRequest, Renderer, probe, sha256_file, thumbnail,
};

struct Source {
    path: PathBuf,
    checksum: shorts_domain::Checksum,
    props: MediaProperties,
}

async fn source(
    tools: &MediaTools,
    dir: &Path,
    seconds: u32,
    w: u32,
    h: u32,
    audio: bool,
) -> Source {
    let path = dir.join(format!("source-{w}x{h}-{seconds}-{audio}.mp4"));
    sample_video(tools, &path, seconds, w, h, audio).await;
    let (checksum, _) = sha256_file(&path).await.unwrap();
    let props = probe(tools, &path).await.unwrap();
    Source {
        path,
        checksum,
        props,
    }
}

fn renderer(tools: &MediaTools, dir: &Path) -> Renderer {
    Renderer::new(tools.clone(), RenderCache::new(dir.join("cache")), 2)
}

async fn render(r: &Renderer, src: &Source, recipe: &RenderRecipe) -> shorts_media::RenderResult {
    r.render(
        &RenderRequest {
            source_path: &src.path,
            source_checksum: &src.checksum,
            source: &src.props,
            recipe,
        },
        |_| {},
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn probes_a_generated_clip() {
    let Some(tools) = tools_or_skip("probes_a_generated_clip") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = source(&tools, dir.path(), 2, 640, 360, true).await;
    assert_eq!((src.props.width, src.props.height), (640, 360));
    assert!(
        (1900..=2100).contains(&src.props.duration_ms),
        "{:?}",
        src.props
    );
    assert_eq!(src.props.video_codec.as_deref(), Some("h264"));
    assert_eq!(src.props.audio_streams, 1);
    assert!((src.props.frame_rate.unwrap() - 30.0).abs() < 0.01);
}

#[tokio::test]
async fn default_preset_renders_vertical_h264_aac_and_caches() {
    let Some(tools) = tools_or_skip("default_preset_renders_vertical_h264_aac_and_caches") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = source(&tools, dir.path(), 2, 640, 360, true).await;
    let r = renderer(&tools, dir.path());
    let progress = Mutex::new(Vec::new());

    let recipe = RenderRecipe::default();
    let first = r
        .render(
            &RenderRequest {
                source_path: &src.path,
                source_checksum: &src.checksum,
                source: &src.props,
                recipe: &recipe,
            },
            |p| progress.lock().unwrap().push(p),
        )
        .await
        .unwrap();
    assert!(!first.from_cache);
    let p = &first.properties;
    assert_eq!((p.width, p.height), (1080, 1920));
    assert_eq!(p.video_codec.as_deref(), Some("h264"));
    assert_eq!(p.audio_codec.as_deref(), Some("aac"));
    assert!(p.container.as_deref().unwrap().contains("mp4"));
    assert!(first.path.starts_with(dir.path().join("cache")));
    assert_eq!(progress.lock().unwrap().last(), Some(&1.0));

    let second = render(&r, &src, &recipe).await;
    assert!(second.from_cache);
    assert_eq!(second.cache_key, first.cache_key);
    assert_eq!(second.path, first.path);

    // The original is untouched.
    assert_eq!(sha256_file(&src.path).await.unwrap().0, src.checksum);
}

#[tokio::test]
async fn trim_crop_fill_and_mute() {
    let Some(tools) = tools_or_skip("trim_crop_fill_and_mute") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = source(&tools, dir.path(), 4, 640, 360, true).await;
    let r = renderer(&tools, dir.path());
    let recipe = RenderRecipe {
        trim: Some(TrimRange {
            start_ms: 1_000,
            end_ms: 2_500,
        }),
        crop: Some(CropRect {
            x: 200,
            y: 0,
            width: 240,
            height: 360,
        }),
        fit: FitMode::Fill,
        audio: AudioChoice::Mute,
        ..Default::default()
    };
    let out = render(&r, &src, &recipe).await;
    let p = &out.properties;
    assert_eq!((p.width, p.height), (1080, 1920));
    assert!(
        (1400..=1600).contains(&p.duration_ms),
        "duration {}",
        p.duration_ms
    );
    assert_eq!(p.audio_streams, 0);
    assert_ne!(
        out.cache_key,
        r.cache_key(&RenderRequest {
            source_path: &src.path,
            source_checksum: &src.checksum,
            source: &src.props,
            recipe: &RenderRecipe::default(),
        })
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn replacement_audio_is_padded_to_the_video() {
    let Some(tools) = tools_or_skip("replacement_audio_is_padded_to_the_video") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = source(&tools, dir.path(), 3, 640, 360, false).await;
    let music = source(&tools, dir.path(), 1, 320, 240, true).await;
    let r = renderer(&tools, dir.path());
    let recipe = RenderRecipe {
        audio: AudioChoice::Replace {
            path: music.path.to_string_lossy().into_owned(),
        },
        ..Default::default()
    };
    let out = render(&r, &src, &recipe).await;
    assert_eq!(out.properties.audio_codec.as_deref(), Some("aac"));
    assert!(
        (2800..=3200).contains(&out.properties.duration_ms),
        "duration {}",
        out.properties.duration_ms
    );
}

fn has_subtitles_filter(tools: &MediaTools) -> bool {
    let out = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-filters"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(" subtitles ")
}

#[tokio::test]
async fn burned_in_subtitles_with_awkward_file_names() {
    let Some(tools) = tools_or_skip("burned_in_subtitles_with_awkward_file_names") else {
        return;
    };
    if !has_subtitles_filter(&tools) {
        eprintln!("skipping: this FFmpeg build has no subtitles filter (libass)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = source(&tools, dir.path(), 2, 640, 360, true).await;
    let subs_dir = dir.path().join("it's [a], b; c");
    std::fs::create_dir_all(&subs_dir).unwrap();
    let name = if cfg!(windows) {
        "subs.srt"
    } else {
        "en:subs.srt"
    };
    let subs = subs_dir.join(name);
    std::fs::write(&subs, "1\n00:00:00,000 --> 00:00:01,500\nHello there\n").unwrap();
    let r = renderer(&tools, dir.path());
    for mode in [SubtitleMode::BurnIn, SubtitleMode::Soft] {
        let recipe = RenderRecipe {
            subtitles: Some(SubtitleTrack {
                path: subs.to_string_lossy().into_owned(),
                mode,
                language: Some("eng".into()),
            }),
            ..Default::default()
        };
        let out = render(&r, &src, &recipe).await;
        assert_eq!(out.properties.width, 1080);
    }
}

#[tokio::test]
async fn editing_the_subtitle_file_invalidates_the_cache() {
    let Some(tools) = tools_or_skip("editing_the_subtitle_file_invalidates_the_cache") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = source(&tools, dir.path(), 1, 320, 240, false).await;
    let subs = dir.path().join("subs.srt");
    std::fs::write(&subs, "1\n00:00:00,000 --> 00:00:00,500\nOne\n").unwrap();
    let r = renderer(&tools, dir.path());
    let recipe = RenderRecipe {
        subtitles: Some(SubtitleTrack {
            path: subs.to_string_lossy().into_owned(),
            mode: SubtitleMode::Soft,
            language: None,
        }),
        ..Default::default()
    };
    let req = RenderRequest {
        source_path: &src.path,
        source_checksum: &src.checksum,
        source: &src.props,
        recipe: &recipe,
    };
    let before = r.cache_key(&req).await.unwrap();
    std::fs::write(&subs, "1\n00:00:00,000 --> 00:00:00,500\nTwo\n").unwrap();
    assert_ne!(before, r.cache_key(&req).await.unwrap());
}

#[tokio::test]
async fn thumbnails_are_cached_jpegs() {
    let Some(tools) = tools_or_skip("thumbnails_are_cached_jpegs") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = source(&tools, dir.path(), 2, 640, 360, false).await;
    let cache = RenderCache::new(dir.path().join("cache"));
    let path = thumbnail(&tools, &cache, &src.path, &src.checksum, 500)
        .await
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG magic");
    let again = thumbnail(&tools, &cache, &src.path, &src.checksum, 500)
        .await
        .unwrap();
    assert_eq!(path, again);
}

#[tokio::test]
async fn ffmpeg_errors_are_reported_and_leave_no_partial_files() {
    let Some(tools) = tools_or_skip("ffmpeg_errors_are_reported_and_leave_no_partial_files") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut src = source(&tools, dir.path(), 1, 320, 240, false).await;
    // Corrupt the bytes but keep the (now wrong) metadata, as if the file was damaged on disk.
    std::fs::write(&src.path, b"not a video").unwrap();
    src.props.duration_ms = 1000;
    let r = renderer(&tools, dir.path());
    let err = r
        .render(
            &RenderRequest {
                source_path: &src.path,
                source_checksum: &src.checksum,
                source: &src.props,
                recipe: &RenderRecipe::default(),
            },
            |_| {},
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, shorts_media::MediaError::Failed { .. }),
        "{err}"
    );
    let leftovers: Vec<_> = walk(&dir.path().join("cache"))
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
