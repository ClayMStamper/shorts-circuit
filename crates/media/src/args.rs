//! FFmpeg argument construction for render recipes. Pure functions, so the exact command line
//! is unit-tested without FFmpeg installed.

use std::ffi::OsString;
use std::path::Path;

use shorts_domain::{AudioChoice, FitMode, MediaProperties, RenderRecipe, SubtitleMode};

/// Bumped whenever this module changes the output for an unchanged recipe, so cached renders
/// from an older pipeline are not reused.
pub const PIPELINE_VERSION: u32 = 1;

/// Builds the FFmpeg command line rendering `input` through `recipe` into `output` (MP4).
pub fn render_args(
    input: &Path,
    source: &MediaProperties,
    recipe: &RenderRecipe,
    output: &Path,
) -> Vec<OsString> {
    let enc = &recipe.encoding;
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |a: &str| args.push(OsString::from(a));
    for a in [
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
    ] {
        push(a);
    }

    // Input 0: the original, seeked and trimmed.
    if let Some(trim) = recipe.trim {
        push("-ss");
        push(&seconds(trim.start_ms));
    }
    args.push("-i".into());
    args.push(input.as_os_str().to_owned());
    let mut next_input = 1;

    let audio_input = match &recipe.audio {
        AudioChoice::Replace { path } => {
            args.push("-i".into());
            args.push(path.into());
            let index = next_input;
            next_input += 1;
            Some(index)
        }
        _ => None,
    };
    let soft_subtitle_input = match &recipe.subtitles {
        Some(subs) if subs.mode == SubtitleMode::Soft => {
            args.push("-i".into());
            args.push(subs.path.clone().into());
            Some(next_input)
        }
        _ => None,
    };

    // Video chain.
    let mut video = Vec::new();
    if let Some(crop) = recipe.crop {
        video.push(format!(
            "crop={}:{}:{}:{}",
            crop.width, crop.height, crop.x, crop.y
        ));
    }
    let (w, h) = (enc.width, enc.height);
    match recipe.fit {
        FitMode::Pad => {
            video.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease"
            ));
            video.push(format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black"));
        }
        FitMode::Fill => {
            video.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=increase"
            ));
            video.push(format!("crop={w}:{h}"));
        }
    }
    video.push("setsar=1".into());
    video.push(format!("fps={}", enc.fps));
    if let Some(subs) = &recipe.subtitles
        && subs.mode == SubtitleMode::BurnIn
    {
        video.push(format!("subtitles=filename={}", filter_path(&subs.path)));
    }
    video.push("format=yuv420p".into());

    let mut graph = format!("[0:v:0]{}[vout]", video.join(","));
    if let Some(index) = audio_input {
        // Cut or pad (with silence) the replacement track to exactly the video's length.
        let d = seconds(recipe.output_duration_ms(source.duration_ms));
        graph.push_str(&format!(
            ";[{index}:a:0]atrim=end={d},apad=whole_dur={d},asetpts=N/SR/TB[aout]"
        ));
    }
    args.push("-filter_complex".into());
    args.push(graph.into());
    args.push("-map".into());
    args.push("[vout]".into());

    let mut push = |a: &str| args.push(OsString::from(a));
    let has_audio = match &recipe.audio {
        AudioChoice::Mute => false,
        AudioChoice::Replace { .. } => {
            push("-map");
            push("[aout]");
            true
        }
        AudioChoice::Original { track } => {
            if source.has_audio() {
                push("-map");
                push(&format!("0:a:{track}?"));
                true
            } else {
                false
            }
        }
    };
    if let Some(index) = soft_subtitle_input {
        push("-map");
        push(&format!("{index}:s:0"));
        push("-c:s");
        push("mov_text");
        if let Some(lang) = recipe
            .subtitles
            .as_ref()
            .and_then(|s| s.language.as_deref())
        {
            push("-metadata:s:s:0");
            push(&format!("language={lang}"));
        }
    }

    if let Some(trim) = recipe.trim {
        push("-t");
        push(&seconds(trim.duration_ms()));
    }

    for a in [
        "-c:v",
        "libx264",
        "-preset",
        &enc.preset,
        "-crf",
        &enc.crf.to_string(),
        "-profile:v",
        "high",
        "-pix_fmt",
        "yuv420p",
    ] {
        push(a);
    }
    if has_audio {
        for a in [
            "-c:a",
            "aac",
            "-b:a",
            &format!("{}k", enc.audio_bitrate_kbps),
            "-ar",
            &enc.audio_sample_rate.to_string(),
            "-ac",
            "2",
        ] {
            push(a);
        }
    } else {
        push("-an");
    }
    // Drop source metadata (it can carry location and device details).
    for a in [
        "-map_metadata",
        "-1",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
    ] {
        push(a);
    }
    args.push(output.as_os_str().to_owned());
    args
}

fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Escapes a path for use as a filter option value inside `-filter_complex`.
///
/// Two escaping levels apply: the option value (`\` `:` `'` are special) and the filtergraph
/// (`\` `'` `[` `]` `,` `;` are special). Backslashes become forward slashes, which FFmpeg
/// accepts on Windows too, so drive letters end up as `C\\:/...`.
pub fn filter_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut option_level = String::with_capacity(normalized.len());
    for c in normalized.chars() {
        if matches!(c, ':' | '\'' | '\\') {
            option_level.push('\\');
        }
        option_level.push(c);
    }
    let mut graph_level = String::with_capacity(option_level.len());
    for c in option_level.chars() {
        if matches!(c, '\\' | '\'' | '[' | ']' | ',' | ';') {
            graph_level.push('\\');
        }
        graph_level.push(c);
    }
    graph_level
}

#[cfg(test)]
mod tests {
    use super::*;
    use shorts_domain::{CropRect, SubtitleTrack, TrimRange};

    fn source() -> MediaProperties {
        MediaProperties {
            duration_ms: 20_000,
            width: 1920,
            height: 1080,
            audio_streams: 1,
            ..Default::default()
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn value_after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let i = args.iter().position(|a| a == flag).expect(flag);
        &args[i + 1]
    }

    #[test]
    fn default_preset_is_vertical_h264_aac_mp4() {
        let args = strings(&render_args(
            Path::new("in.mov"),
            &source(),
            &RenderRecipe::default(),
            Path::new("out.mp4"),
        ));
        assert_eq!(
            value_after(&args, "-filter_complex"),
            "[0:v:0]scale=1080:1920:force_original_aspect_ratio=decrease,\
             pad=1080:1920:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps=30,format=yuv420p[vout]"
        );
        assert_eq!(value_after(&args, "-c:v"), "libx264");
        assert_eq!(value_after(&args, "-crf"), "20");
        assert_eq!(value_after(&args, "-c:a"), "aac");
        assert_eq!(value_after(&args, "-b:a"), "128k");
        assert_eq!(value_after(&args, "-ar"), "48000");
        assert_eq!(value_after(&args, "-f"), "mp4");
        assert_eq!(args.last().unwrap(), "out.mp4");
        assert!(args.contains(&"+faststart".to_owned()));
        assert!(args.contains(&"0:a:0?".to_owned()));
        assert!(!args.contains(&"-ss".to_owned()));
    }

    #[test]
    fn trim_crop_and_fill() {
        let recipe = RenderRecipe {
            trim: Some(TrimRange {
                start_ms: 1_500,
                end_ms: 9_250,
            }),
            crop: Some(CropRect {
                x: 420,
                y: 0,
                width: 1080,
                height: 1080,
            }),
            fit: FitMode::Fill,
            ..Default::default()
        };
        let args = strings(&render_args(
            Path::new("in.mp4"),
            &source(),
            &recipe,
            Path::new("out.mp4"),
        ));
        // Seek before the input, duration as an output option.
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < input);
        assert_eq!(value_after(&args, "-ss"), "1.500");
        assert_eq!(value_after(&args, "-t"), "7.750");
        assert!(value_after(&args, "-filter_complex").starts_with(
            "[0:v:0]crop=1080:1080:420:0,scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920"
        ));
    }

    #[test]
    fn mute_drops_audio() {
        let recipe = RenderRecipe {
            audio: AudioChoice::Mute,
            ..Default::default()
        };
        let args = strings(&render_args(
            Path::new("in.mp4"),
            &source(),
            &recipe,
            Path::new("out.mp4"),
        ));
        assert!(args.contains(&"-an".to_owned()));
        assert!(!args.contains(&"-c:a".to_owned()));
    }

    #[test]
    fn silent_sources_get_no_audio_map() {
        let silent = MediaProperties {
            audio_streams: 0,
            ..source()
        };
        let args = strings(&render_args(
            Path::new("in.mp4"),
            &silent,
            &RenderRecipe::default(),
            Path::new("out.mp4"),
        ));
        assert!(args.contains(&"-an".to_owned()));
    }

    #[test]
    fn replacement_audio_is_a_second_input_padded_to_the_video() {
        let recipe = RenderRecipe {
            audio: AudioChoice::Replace {
                path: "music.m4a".into(),
            },
            ..Default::default()
        };
        let args = strings(&render_args(
            Path::new("in.mp4"),
            &source(),
            &recipe,
            Path::new("out.mp4"),
        ));
        let inputs: Vec<_> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "-i")
            .map(|(i, _)| args[i + 1].as_str())
            .collect();
        assert_eq!(inputs, vec!["in.mp4", "music.m4a"]);
        assert!(
            value_after(&args, "-filter_complex")
                .ends_with(";[1:a:0]atrim=end=20.000,apad=whole_dur=20.000,asetpts=N/SR/TB[aout]")
        );
        assert!(args.contains(&"[aout]".to_owned()));
    }

    #[test]
    fn burned_in_subtitles_follow_scaling() {
        let recipe = RenderRecipe {
            subtitles: Some(SubtitleTrack {
                path: "subs/en.srt".into(),
                mode: SubtitleMode::BurnIn,
                language: None,
            }),
            ..Default::default()
        };
        let args = strings(&render_args(
            Path::new("in.mp4"),
            &source(),
            &recipe,
            Path::new("out.mp4"),
        ));
        assert!(
            value_after(&args, "-filter_complex")
                .contains("fps=30,subtitles=filename=subs/en.srt,format=yuv420p")
        );
    }

    #[test]
    fn soft_subtitles_become_a_mov_text_track() {
        let recipe = RenderRecipe {
            subtitles: Some(SubtitleTrack {
                path: "en.srt".into(),
                mode: SubtitleMode::Soft,
                language: Some("eng".into()),
            }),
            ..Default::default()
        };
        let args = strings(&render_args(
            Path::new("in.mp4"),
            &source(),
            &recipe,
            Path::new("out.mp4"),
        ));
        assert!(args.contains(&"1:s:0".to_owned()));
        assert_eq!(value_after(&args, "-c:s"), "mov_text");
        assert_eq!(value_after(&args, "-metadata:s:s:0"), "language=eng");
        assert!(!value_after(&args, "-filter_complex").contains("subtitles="));
    }

    #[test]
    fn windows_paths_are_escaped_for_filters() {
        assert_eq!(
            filter_path(r"C:\Users\me\subs.srt"),
            r"C\\:/Users/me/subs.srt"
        );
        assert_eq!(filter_path("it's [1].srt"), r"it\\\'s \[1\].srt");
        assert_eq!(filter_path("a,b;c.srt"), r"a\,b\;c.srt");
    }
}
