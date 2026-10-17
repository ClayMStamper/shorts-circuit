import { describe, expect, it } from "vitest";
import { centreCrop, cloneRecipe, defaultRecipe, outputDuration, recipeIssues } from "./recipe";
import { captionLength, fullCaption } from "./limits";
import type { MediaProperties } from "./types";

const landscape: MediaProperties = {
  duration_ms: 20_000,
  width: 1920,
  height: 1080,
  rotation: 0,
  frame_rate: 30,
  video_codec: "h264",
  audio_codec: "aac",
  audio_streams: 1,
  container: "mp4",
  bit_rate: null,
};

describe("recipes", () => {
  it("defaults to the vertical 1080x1920 preset and validates cleanly", () => {
    const r = defaultRecipe();
    expect(r.encoding).toMatchObject({ width: 1080, height: 1920, fps: 30 });
    expect(r.audio).toEqual({ kind: "original", track: 0 });
    expect(recipeIssues(r, landscape)).toEqual([]);
  });

  it("flags trims and crops outside the source", () => {
    const r = defaultRecipe();
    r.trim = { start_ms: 5_000, end_ms: 25_000 };
    r.crop = { x: 1500, y: 0, width: 608, height: 1080 };
    expect(recipeIssues(r, landscape)).toEqual([
      "Trim end is past the end of the video.",
      "Crop goes outside the 1920x1080 frame.",
    ]);
  });

  it("accounts for rotation when checking crops", () => {
    const rotated = { ...landscape, rotation: 90 };
    const r = defaultRecipe();
    r.crop = { x: 0, y: 0, width: 1080, height: 1920 };
    expect(recipeIssues(r, rotated)).toEqual([]);
  });

  it("rejects odd dimensions and unknown presets", () => {
    const r = defaultRecipe();
    r.encoding.width = 1081;
    r.encoding.preset = "warp";
    const issues = recipeIssues(r, null);
    expect(issues).toContain("Width and height must be even.");
    expect(issues).toContain("Unknown encoder preset.");
  });

  it("computes the trimmed duration", () => {
    const r = defaultRecipe();
    expect(outputDuration(r, 20_000)).toBe(20_000);
    r.trim = { start_ms: 2_000, end_ms: 30_000 };
    expect(outputDuration(r, 20_000)).toBe(18_000);
  });

  it("centre-crops to the output aspect ratio", () => {
    const crop = centreCrop(landscape, defaultRecipe());
    expect(crop).toEqual({ x: 656, y: 0, width: 608, height: 1080 });
    expect(recipeIssues({ ...defaultRecipe(), crop }, landscape)).toEqual([]);
  });
});

describe("captions", () => {
  it("joins copy and hashtags like the backend", () => {
    expect(fullCaption(" Hi ", ["a", "b"])).toBe("Hi\n\n#a #b");
    expect(fullCaption("", ["a"])).toBe("#a");
    expect(captionLength("héllo", [])).toBe(5);
  });
});

describe("cloneRecipe", () => {
  it("deep-copies, including reactive proxies", () => {
    const original = defaultRecipe();
    const proxy = new Proxy(original, {});
    const copy = cloneRecipe(proxy);
    copy.encoding.crf = 30;
    expect(original.encoding.crf).toBe(20);
    expect(copy).not.toBe(original);
  });
});
