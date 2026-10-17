//! Helpers for tests (here and in downstream crates) that need real media.

use std::path::Path;
use std::time::Duration;

use crate::process;
use crate::tools::MediaTools;

/// The FFmpeg tools, or `None` with a skip message when they aren't installed. Tests should
/// `return` early on `None`.
pub fn tools_or_skip(test: &str) -> Option<MediaTools> {
    match MediaTools::discover(None) {
        Ok(tools) => Some(tools),
        Err(e) => {
            eprintln!("skipping {test}: {e}");
            None
        }
    }
}

/// Writes a synthetic test clip (colour bars, optional 440 Hz tone) with FFmpeg's lavfi inputs.
pub async fn sample_video(
    tools: &MediaTools,
    path: &Path,
    seconds: u32,
    width: u32,
    height: u32,
    with_audio: bool,
) {
    let mut args: Vec<std::ffi::OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("testsrc2=size={width}x{height}:rate=30:duration={seconds}").into(),
    ];
    if with_audio {
        args.extend([
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("sine=frequency=440:sample_rate=48000:duration={seconds}").into(),
        ]);
    }
    args.extend([
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "ultrafast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
    ]);
    if with_audio {
        args.extend(["-c:a".into(), "aac".into(), "-shortest".into()]);
    }
    args.push(path.as_os_str().to_owned());
    process::run(
        "ffmpeg",
        &tools.ffmpeg,
        &args,
        Duration::from_secs(120),
        |_| {},
    )
    .await
    .expect("generating a sample video");
}
