//! FFprobe inspection.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use shorts_domain::MediaProperties;

use crate::process;
use crate::tools::MediaTools;
use crate::{MediaError, MediaResult};

const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Reads the technical properties of a media file.
pub async fn probe(tools: &MediaTools, path: &Path) -> MediaResult<MediaProperties> {
    if !path.is_file() {
        return Err(MediaError::MissingInput(path.to_owned()));
    }
    let args: Vec<OsString> = [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ]
    .iter()
    .map(OsString::from)
    .chain(std::iter::once(path.as_os_str().to_owned()))
    .collect();
    let out = process::run("ffprobe", &tools.ffprobe, &args, PROBE_TIMEOUT, |_| {}).await?;
    parse(&String::from_utf8_lossy(&out.stdout))
}

#[derive(Deserialize)]
struct Report {
    #[serde(default)]
    streams: Vec<Stream>,
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
    #[serde(default)]
    disposition: std::collections::HashMap<String, i64>,
}

#[derive(Deserialize)]
struct SideData {
    rotation: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Format {
    duration: Option<String>,
    bit_rate: Option<String>,
    format_name: Option<String>,
}

/// Parses `ffprobe -print_format json -show_format -show_streams` output.
pub fn parse(json: &str) -> MediaResult<MediaProperties> {
    let report: Report =
        serde_json::from_str(json).map_err(|e| MediaError::Probe(e.to_string()))?;
    let video = report.streams.iter().find(|s| {
        s.codec_type.as_deref() == Some("video")
            // Cover art is reported as a video stream; skip it.
            && s.disposition.get("attached_pic").copied().unwrap_or(0) == 0
    });
    let video = video.ok_or_else(|| MediaError::Probe("no video stream".into()))?;
    let audio_streams = report
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .count();
    let audio = report
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"));

    let duration_s = report
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .or(video.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);
    let rotation = video
        .side_data_list
        .iter()
        .find_map(|sd| sd.rotation.as_ref().and_then(serde_json::Value::as_f64))
        .or_else(|| video.tags.get("rotate").and_then(|r| r.parse::<f64>().ok()))
        .unwrap_or(0.0);

    Ok(MediaProperties {
        duration_ms: (duration_s * 1000.0).round().max(0.0) as u64,
        width: video.width.unwrap_or(0),
        height: video.height.unwrap_or(0),
        rotation: normalize_rotation(rotation),
        frame_rate: video
            .avg_frame_rate
            .as_deref()
            .and_then(parse_rate)
            .or_else(|| video.r_frame_rate.as_deref().and_then(parse_rate)),
        video_codec: video.codec_name.clone(),
        audio_codec: audio.and_then(|a| a.codec_name.clone()),
        audio_streams: u32::try_from(audio_streams).unwrap_or(u32::MAX),
        container: report.format.as_ref().and_then(|f| f.format_name.clone()),
        bit_rate: report
            .format
            .as_ref()
            .and_then(|f| f.bit_rate.as_deref())
            .and_then(|b| b.parse().ok()),
    })
}

fn normalize_rotation(degrees: f64) -> i32 {
    let r = (degrees.round() as i32).rem_euclid(360);
    // Snap to the nearest quarter turn.
    ((r + 45) / 90 * 90) % 360
}

fn parse_rate(rate: &str) -> Option<f64> {
    let (num, den) = rate.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    (den > 0.0 && num > 0.0).then(|| num / den)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE_CLIP: &str = r#"{
      "streams": [
        {"index":0,"codec_name":"hevc","codec_type":"video","width":1920,"height":1080,
         "r_frame_rate":"30/1","avg_frame_rate":"30000/1001","duration":"12.512",
         "side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}],
         "disposition":{"default":1,"attached_pic":0}},
        {"index":1,"codec_name":"aac","codec_type":"audio","duration":"12.500",
         "disposition":{"default":1,"attached_pic":0}},
        {"index":2,"codec_name":"mjpeg","codec_type":"video","width":320,"height":320,
         "disposition":{"default":0,"attached_pic":1}}
      ],
      "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"12.512000","bit_rate":"8000000"}
    }"#;

    #[test]
    fn parses_a_rotated_phone_clip() {
        let p = parse(PHONE_CLIP).unwrap();
        assert_eq!(p.duration_ms, 12_512);
        assert_eq!((p.width, p.height), (1920, 1080));
        assert_eq!(p.rotation, 270);
        assert_eq!(p.display_dimensions(), (1080, 1920));
        assert!((p.frame_rate.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(p.video_codec.as_deref(), Some("hevc"));
        assert_eq!(p.audio_codec.as_deref(), Some("aac"));
        assert_eq!(p.audio_streams, 1);
        assert_eq!(p.bit_rate, Some(8_000_000));
    }

    #[test]
    fn legacy_rotate_tag_is_understood() {
        let json = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":640,
            "height":480,"tags":{"rotate":"90"}}],"format":{"duration":"1.0"}}"#;
        let p = parse(json).unwrap();
        assert_eq!(p.rotation, 90);
        assert_eq!(p.audio_streams, 0);
        assert!(!p.has_audio());
    }

    #[test]
    fn audio_only_files_are_rejected() {
        let json = r#"{"streams":[{"codec_type":"audio","codec_name":"mp3"}],"format":{}}"#;
        assert!(matches!(parse(json), Err(MediaError::Probe(_))));
    }

    #[test]
    fn rotation_is_normalized() {
        assert_eq!(normalize_rotation(-90.0), 270);
        assert_eq!(normalize_rotation(90.0), 90);
        assert_eq!(normalize_rotation(180.0), 180);
        assert_eq!(normalize_rotation(-180.0), 180);
        assert_eq!(normalize_rotation(0.0), 0);
    }
}
