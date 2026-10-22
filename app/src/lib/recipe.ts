// Client-side helpers for editing render recipes. The Rust domain re-validates everything;
// these checks only give immediate feedback while typing.

import type { MediaProperties, RenderRecipe } from "./types";

export const RECIPE_SCHEMA_VERSION = 1;

export const X264_PRESETS = [
  "ultrafast",
  "superfast",
  "veryfast",
  "faster",
  "fast",
  "medium",
  "slow",
  "slower",
  "veryslow",
] as const;

/** The initial preset: vertical 1080x1920 H.264/AAC MP4. */
export function defaultRecipe(): RenderRecipe {
  return {
    schema: RECIPE_SCHEMA_VERSION,
    trim: null,
    crop: null,
    fit: "pad",
    subtitles: null,
    audio: { kind: "original", track: 0 },
    encoding: {
      width: 1080,
      height: 1920,
      fps: 30,
      crf: 20,
      preset: "medium",
      audio_bitrate_kbps: 128,
      audio_sample_rate: 48000,
    },
  };
}

/** A deep copy. Works on Svelte state proxies too (`structuredClone` rejects those). */
export function cloneRecipe(recipe: RenderRecipe): RenderRecipe {
  return JSON.parse(JSON.stringify(recipe)) as RenderRecipe;
}

function displayDimensions(p: MediaProperties): [number, number] {
  const rotated = ((p.rotation % 180) + 180) % 180 === 90;
  return rotated ? [p.height, p.width] : [p.width, p.height];
}

/** Human-readable problems with a recipe for the given source. Empty means it looks fine. */
export function recipeIssues(recipe: RenderRecipe, source: MediaProperties | null): string[] {
  const issues: string[] = [];
  if (recipe.trim) {
    if (recipe.trim.end_ms <= recipe.trim.start_ms) issues.push("Trim end must be after the start.");
    if (source && recipe.trim.end_ms > source.duration_ms)
      issues.push("Trim end is past the end of the video.");
  }
  if (recipe.crop) {
    const c = recipe.crop;
    if (c.width <= 0 || c.height <= 0) issues.push("Crop width and height must be positive.");
    if (source) {
      const [w, h] = displayDimensions(source);
      if (c.x + c.width > w || c.y + c.height > h) issues.push(`Crop goes outside the ${w}x${h} frame.`);
    }
  }
  if (recipe.subtitles && recipe.subtitles.path.trim() === "") issues.push("Choose a subtitle file.");
  if (recipe.audio.kind === "replace" && recipe.audio.path.trim() === "")
    issues.push("Choose a replacement audio file.");
  if (recipe.audio.kind === "original" && source && source.audio_streams > 0 && recipe.audio.track >= source.audio_streams)
    issues.push(`The source has ${source.audio_streams} audio track(s).`);
  const e = recipe.encoding;
  if (e.width % 2 !== 0 || e.height % 2 !== 0) issues.push("Width and height must be even.");
  if (e.width < 144 || e.width > 4096 || e.height < 144 || e.height > 4096)
    issues.push("Width and height must be between 144 and 4096.");
  if (e.fps < 1 || e.fps > 120) issues.push("Frame rate must be between 1 and 120.");
  if (e.crf < 0 || e.crf > 51) issues.push("Quality (CRF) must be between 0 and 51.");
  if (!(X264_PRESETS as readonly string[]).includes(e.preset)) issues.push("Unknown encoder preset.");
  if (e.audio_bitrate_kbps < 32 || e.audio_bitrate_kbps > 512)
    issues.push("Audio bitrate must be between 32 and 512 kbps.");
  if (e.audio_sample_rate !== 44100 && e.audio_sample_rate !== 48000)
    issues.push("Audio sample rate must be 44100 or 48000.");
  return issues;
}

/** Output duration after trimming. */
export function outputDuration(recipe: RenderRecipe, sourceMs: number): number {
  if (!recipe.trim) return sourceMs;
  return Math.max(0, Math.min(recipe.trim.end_ms, sourceMs) - recipe.trim.start_ms);
}

/** A centred crop of the source at the output's aspect ratio (e.g. 9:16 from a 16:9 clip). */
export function centreCrop(source: MediaProperties, recipe: RenderRecipe) {
  const [w, h] = displayDimensions(source);
  const target = recipe.encoding.width / recipe.encoding.height;
  let cw = w;
  let ch = Math.round(w / target);
  if (ch > h) {
    ch = h;
    cw = Math.round(h * target);
  }
  cw -= cw % 2;
  ch -= ch % 2;
  return { x: Math.floor((w - cw) / 2), y: Math.floor((h - ch) / 2), width: cw, height: ch };
}
