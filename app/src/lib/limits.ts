// Indicative caption limits, mirroring crates/domain/src/platform.rs, for live counters.

import type { Platform } from "./types";

export const CAPTION_LIMITS: Record<Platform, { chars: number; hashtags: number }> = {
  youtube_shorts: { chars: 5000, hashtags: 15 },
  instagram_reels: { chars: 2200, hashtags: 30 },
  snapchat_spotlight: { chars: 160, hashtags: 10 },
  linkedin: { chars: 3000, hashtags: 30 },
};

/** The caption as posted: text, a blank line, then `#tag` words. */
export function fullCaption(caption: string, hashtags: string[]): string {
  const tags = hashtags.map((t) => `#${t}`).join(" ");
  const text = caption.trim();
  if (!text) return tags;
  if (!tags) return text;
  return `${text}\n\n${tags}`;
}

export function captionLength(caption: string, hashtags: string[]): number {
  return [...fullCaption(caption, hashtags)].length;
}
