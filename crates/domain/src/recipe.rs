//! Versioned render recipes: the edit applied to an original to produce a variant.
//!
//! A recipe is pure data. `shorts-media` turns it into FFmpeg arguments and keys its render
//! cache on the recipe's canonical JSON, so any field change produces a different render.

use serde::{Deserialize, Serialize};

use crate::media::MediaProperties;

/// Bump when the recipe format changes in a way that alters render output.
pub const RECIPE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl TrimRange {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// A crop rectangle in displayed pixels, i.e. after the source's rotation metadata is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// How the (cropped) picture is fitted into the output frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// Scale to fit inside the frame and letterbox with black bars.
    #[default]
    Pad,
    /// Scale to cover the frame and crop the overflow from the centre.
    Fill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleMode {
    /// Render the text into the picture. Works on every platform.
    #[default]
    BurnIn,
    /// Add a selectable text track (`mov_text`). Not every platform keeps it.
    Soft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleTrack {
    /// Path to an `.srt` / `.ass` / `.vtt` file.
    pub path: String,
    pub mode: SubtitleMode,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioChoice {
    /// Keep an audio stream from the original (0 = first audio stream).
    Original { track: u32 },
    /// Drop audio entirely (e.g. when the sound is licensed for the origin platform only).
    Mute,
    /// Replace the soundtrack with another file.
    Replace { path: String },
}

impl Default for AudioChoice {
    fn default() -> Self {
        Self::Original { track: 0 }
    }
}

/// Output encoding. The default is the initial preset: vertical 1080x1920 H.264/AAC MP4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodingSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// x264 constant rate factor (0-51, lower is higher quality).
    pub crf: u8,
    /// x264 preset name.
    pub preset: String,
    pub audio_bitrate_kbps: u32,
    pub audio_sample_rate: u32,
}

impl Default for EncodingSettings {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            fps: 30,
            crf: 20,
            preset: "medium".to_owned(),
            audio_bitrate_kbps: 128,
            audio_sample_rate: 48_000,
        }
    }
}

const X264_PRESETS: [&str; 10] = [
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipeIssue {
    UnsupportedSchema { found: u32 },
    EmptyTrim,
    TrimBeyondSource { end_ms: u64, duration_ms: u64 },
    EmptyCrop,
    CropOutsideSource,
    OddDimensions,
    DimensionsOutOfRange,
    FrameRateOutOfRange,
    CrfOutOfRange,
    UnknownPreset { preset: String },
    AudioBitrateOutOfRange,
    UnsupportedSampleRate,
    MissingAudioTrack { track: u32, available: u32 },
    EmptyPath,
}

/// The edit applied to an original to produce a variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderRecipe {
    pub schema: u32,
    pub trim: Option<TrimRange>,
    pub crop: Option<CropRect>,
    pub fit: FitMode,
    pub subtitles: Option<SubtitleTrack>,
    pub audio: AudioChoice,
    pub encoding: EncodingSettings,
}

impl Default for RenderRecipe {
    fn default() -> Self {
        Self {
            schema: RECIPE_SCHEMA_VERSION,
            trim: None,
            crop: None,
            fit: FitMode::default(),
            subtitles: None,
            audio: AudioChoice::default(),
            encoding: EncodingSettings::default(),
        }
    }
}

impl RenderRecipe {
    /// The duration of the rendered output for a source of the given length.
    pub fn output_duration_ms(&self, source_duration_ms: u64) -> u64 {
        match self.trim {
            Some(trim) => trim
                .end_ms
                .min(source_duration_ms)
                .saturating_sub(trim.start_ms),
            None => source_duration_ms,
        }
    }

    /// Checks the recipe on its own and, when given, against the source's properties.
    pub fn validate(&self, source: Option<&MediaProperties>) -> Vec<RecipeIssue> {
        let mut issues = Vec::new();
        if self.schema != RECIPE_SCHEMA_VERSION {
            issues.push(RecipeIssue::UnsupportedSchema { found: self.schema });
        }
        if let Some(trim) = self.trim {
            if trim.end_ms <= trim.start_ms {
                issues.push(RecipeIssue::EmptyTrim);
            }
            if let Some(src) = source
                && trim.end_ms > src.duration_ms
            {
                issues.push(RecipeIssue::TrimBeyondSource {
                    end_ms: trim.end_ms,
                    duration_ms: src.duration_ms,
                });
            }
        }
        if let Some(crop) = self.crop {
            if crop.width == 0 || crop.height == 0 {
                issues.push(RecipeIssue::EmptyCrop);
            }
            if let Some(src) = source {
                let (w, h) = src.display_dimensions();
                if crop.x.saturating_add(crop.width) > w || crop.y.saturating_add(crop.height) > h {
                    issues.push(RecipeIssue::CropOutsideSource);
                }
            }
        }
        if let Some(subs) = &self.subtitles
            && subs.path.trim().is_empty()
        {
            issues.push(RecipeIssue::EmptyPath);
        }
        match &self.audio {
            AudioChoice::Original { track } => {
                if let Some(src) = source
                    && *track >= src.audio_streams
                    && src.audio_streams > 0
                {
                    issues.push(RecipeIssue::MissingAudioTrack {
                        track: *track,
                        available: src.audio_streams,
                    });
                }
            }
            AudioChoice::Replace { path } if path.trim().is_empty() => {
                issues.push(RecipeIssue::EmptyPath);
            }
            AudioChoice::Replace { .. } | AudioChoice::Mute => {}
        }
        let enc = &self.encoding;
        if !enc.width.is_multiple_of(2) || !enc.height.is_multiple_of(2) {
            issues.push(RecipeIssue::OddDimensions);
        }
        if !(144..=4096).contains(&enc.width) || !(144..=4096).contains(&enc.height) {
            issues.push(RecipeIssue::DimensionsOutOfRange);
        }
        if !(1..=120).contains(&enc.fps) {
            issues.push(RecipeIssue::FrameRateOutOfRange);
        }
        if enc.crf > 51 {
            issues.push(RecipeIssue::CrfOutOfRange);
        }
        if !X264_PRESETS.contains(&enc.preset.as_str()) {
            issues.push(RecipeIssue::UnknownPreset {
                preset: enc.preset.clone(),
            });
        }
        if !(32..=512).contains(&enc.audio_bitrate_kbps) {
            issues.push(RecipeIssue::AudioBitrateOutOfRange);
        }
        if ![44_100, 48_000].contains(&enc.audio_sample_rate) {
            issues.push(RecipeIssue::UnsupportedSampleRate);
        }
        issues
    }

    /// Stable JSON used for cache keys. Field order follows the struct definition, so the
    /// same recipe always serializes to the same bytes.
    pub fn canonical_json(&self) -> String {
        serde_json::to_string(self).expect("recipe serialization is infallible")
    }

    /// Whether the output will carry an audio track.
    pub fn produces_audio(&self, source: &MediaProperties) -> bool {
        match self.audio {
            AudioChoice::Mute => false,
            AudioChoice::Replace { .. } => true,
            AudioChoice::Original { .. } => source.has_audio(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> MediaProperties {
        MediaProperties {
            duration_ms: 10_000,
            width: 1920,
            height: 1080,
            audio_streams: 1,
            ..Default::default()
        }
    }

    #[test]
    fn default_recipe_is_the_vertical_h264_preset_and_valid() {
        let recipe = RenderRecipe::default();
        assert_eq!(
            (recipe.encoding.width, recipe.encoding.height),
            (1080, 1920)
        );
        assert!(recipe.validate(Some(&source())).is_empty());
    }

    #[test]
    fn trims_are_checked_against_the_source() {
        let mut recipe = RenderRecipe {
            trim: Some(TrimRange {
                start_ms: 5_000,
                end_ms: 5_000,
            }),
            ..Default::default()
        };
        assert_eq!(recipe.validate(None), vec![RecipeIssue::EmptyTrim]);
        recipe.trim = Some(TrimRange {
            start_ms: 0,
            end_ms: 12_000,
        });
        assert_eq!(
            recipe.validate(Some(&source())),
            vec![RecipeIssue::TrimBeyondSource {
                end_ms: 12_000,
                duration_ms: 10_000
            }]
        );
    }

    #[test]
    fn crops_must_fit_the_source() {
        let recipe = RenderRecipe {
            crop: Some(CropRect {
                x: 1000,
                y: 0,
                width: 1000,
                height: 1080,
            }),
            ..Default::default()
        };
        assert!(recipe.validate(None).is_empty());
        assert_eq!(
            recipe.validate(Some(&source())),
            vec![RecipeIssue::CropOutsideSource]
        );
    }

    #[test]
    fn encoding_settings_are_bounded() {
        let recipe = RenderRecipe {
            encoding: EncodingSettings {
                width: 1081,
                crf: 60,
                preset: "warp".into(),
                audio_sample_rate: 22_050,
                ..Default::default()
            },
            ..Default::default()
        };
        let issues = recipe.validate(None);
        assert!(issues.contains(&RecipeIssue::OddDimensions));
        assert!(issues.contains(&RecipeIssue::CrfOutOfRange));
        assert!(issues.contains(&RecipeIssue::UnknownPreset {
            preset: "warp".into()
        }));
        assert!(issues.contains(&RecipeIssue::UnsupportedSampleRate));
    }

    #[test]
    fn audio_track_must_exist() {
        let recipe = RenderRecipe {
            audio: AudioChoice::Original { track: 2 },
            ..Default::default()
        };
        assert_eq!(
            recipe.validate(Some(&source())),
            vec![RecipeIssue::MissingAudioTrack {
                track: 2,
                available: 1
            }]
        );
    }

    #[test]
    fn unknown_schema_versions_are_rejected() {
        let recipe = RenderRecipe {
            schema: RECIPE_SCHEMA_VERSION + 1,
            ..Default::default()
        };
        assert_eq!(
            recipe.validate(None),
            vec![RecipeIssue::UnsupportedSchema {
                found: RECIPE_SCHEMA_VERSION + 1
            }]
        );
    }

    #[test]
    fn canonical_json_is_stable_and_sensitive_to_changes() {
        let a = RenderRecipe::default();
        let b = RenderRecipe::default();
        assert_eq!(a.canonical_json(), b.canonical_json());
        let c = RenderRecipe {
            audio: AudioChoice::Mute,
            ..Default::default()
        };
        assert_ne!(a.canonical_json(), c.canonical_json());
        let parsed: RenderRecipe = serde_json::from_str(&c.canonical_json()).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn output_duration_respects_trim() {
        let recipe = RenderRecipe {
            trim: Some(TrimRange {
                start_ms: 2_000,
                end_ms: 20_000,
            }),
            ..Default::default()
        };
        assert_eq!(recipe.output_duration_ms(10_000), 8_000);
        assert_eq!(RenderRecipe::default().output_duration_ms(10_000), 10_000);
    }

    #[test]
    fn mute_produces_no_audio() {
        let muted = RenderRecipe {
            audio: AudioChoice::Mute,
            ..Default::default()
        };
        assert!(!muted.produces_audio(&source()));
        assert!(RenderRecipe::default().produces_audio(&source()));
    }
}
