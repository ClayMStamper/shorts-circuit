import type { Action, Platform, PublicationState } from "./types";

export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 100)) / 10;
  const minutes = Math.floor(total / 60);
  const seconds = total - minutes * 60;
  const s = seconds.toFixed(1).padStart(4, "0");
  return `${minutes}:${s}`;
}

export function formatBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[unit]}`;
}

/** Parses `m:ss.s`, `ss.s` or `h:mm:ss` into milliseconds; `null` if malformed. */
export function parseTimecode(input: string): number | null {
  const parts = input.trim().split(":");
  if (parts.length === 0 || parts.length > 3 || parts.some((p) => p.trim() === "")) return null;
  let seconds = 0;
  for (const part of parts) {
    const n = Number(part);
    if (!Number.isFinite(n) || n < 0) return null;
    seconds = seconds * 60 + n;
  }
  return Math.round(seconds * 1000);
}

const STATE_LABELS: Record<PublicationState, string> = {
  draft: "Draft",
  awaiting_approval: "Awaiting approval",
  scheduled: "Scheduled",
  preparing: "Preparing",
  uploading: "Uploading",
  processing: "Processing",
  publishing: "Publishing",
  published: "Published",
  retry_scheduled: "Retry scheduled",
  needs_authentication: "Needs sign-in",
  needs_user_action: "Needs you",
  outcome_unknown: "Outcome unknown",
  failed: "Failed",
  cancelled: "Cancelled",
};

export function stateLabel(state: PublicationState): string {
  return STATE_LABELS[state];
}

/** A coarse tone for badges. */
export function stateTone(state: PublicationState): "neutral" | "info" | "good" | "warn" | "bad" {
  switch (state) {
    case "published":
      return "good";
    case "scheduled":
    case "preparing":
    case "uploading":
    case "processing":
    case "publishing":
    case "retry_scheduled":
      return "info";
    case "awaiting_approval":
    case "needs_authentication":
    case "needs_user_action":
    case "outcome_unknown":
      return "warn";
    case "failed":
      return "bad";
    default:
      return "neutral";
  }
}

const ACTION_LABELS: Record<Action, string> = {
  edit: "Edit",
  submitForReview: "Submit for review",
  review: "Review",
  approve: "Approve",
  revokeApproval: "Revoke approval",
  reschedule: "Reschedule",
  publishNow: "Post now",
  cancel: "Cancel",
  retry: "Retry",
  resolveUserAction: "Retry",
  confirmPosted: "Mark as posted",
  resolveOutcome: "Record outcome",
  exportPackage: "Export package",
  reconnect: "Reconnect…",
  sendToCloud: "Post from the cloud",
  takeBack: "Take back",
  republish: "Republish",
};

export function actionLabel(action: Action): string {
  return ACTION_LABELS[action];
}

const PLATFORM_LABELS: Record<Platform, string> = {
  youtube_shorts: "YouTube Shorts",
  instagram_reels: "Instagram Reels",
  snapchat_spotlight: "Snapchat Spotlight",
  linkedin: "LinkedIn",
};

export function platformLabel(platform: Platform): string {
  return PLATFORM_LABELS[platform];
}

function zonedParts(date: Date, timeZone: string): Record<string, string> {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(date);
  return Object.fromEntries(parts.map((p) => [p.type, p.value]));
}

/** `YYYY-MM-DDTHH:MM` for an instant as seen in `timeZone`, for `<input type="datetime-local">`. */
export function toLocalInput(iso: string, timeZone: string): string {
  const p = zonedParts(new Date(iso), timeZone);
  return `${p.year}-${p.month}-${p.day}T${p.hour}:${p.minute}`;
}

/** A readable instant in `timeZone`, e.g. `Mon 2 Nov 2026, 09:00 EST`. */
export function formatInZone(iso: string, timeZone: string): string {
  return new Intl.DateTimeFormat("en-GB", {
    timeZone,
    weekday: "short",
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
    timeZoneName: "short",
  }).format(new Date(iso));
}

/** An instant in the computer's timezone. */
export function formatDateTime(iso: string): string {
  return formatInZone(iso, systemTimeZone());
}

/** Splits free text into hashtags: `#a b, c` becomes `["a", "b", "c"]`. */
export function parseHashtags(input: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of input.split(/[\s,]+/)) {
    const tag = raw.replace(/^#+/, "").trim();
    if (tag && !seen.has(tag.toLowerCase())) {
      seen.add(tag.toLowerCase());
      out.push(tag);
    }
  }
  return out;
}

/** The detected IANA timezone of this machine. */
export function systemTimeZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone;
}

/** "just now", "5 minutes ago", "15 hours ago", "3 days ago" (relative to `now`). */
export function timeAgo(iso: string, now: Date = new Date()): string {
  const seconds = Math.round((now.getTime() - new Date(iso).getTime()) / 1000);
  if (seconds < 45) return "just now";
  const units: Array<[number, string]> = [
    [60, "minute"],
    [3600, "hour"],
    [86400, "day"],
  ];
  let value = Math.max(1, Math.floor(seconds / 60));
  let unit = "minute";
  for (const [size, name] of units) {
    const v = Math.floor(seconds / size);
    if (v >= 1) {
      value = v;
      unit = name;
    }
  }
  if (unit === "day" && value > 30) return formatDateTime(iso);
  return `${value} ${unit}${value === 1 ? "" : "s"} ago`;
}

/** "Approved by you", "Approved by workflow TikTok → YouTube Shorts", … */
export function approverLabel(by: string | null): string | null {
  if (!by) return null;
  if (by === "owner") return "Approved by you";
  if (by.startsWith("workflow:")) return `Approved by workflow ${by.slice("workflow:".length)}`;
  if (by.startsWith("rule:")) return `Approved by rule ${by.slice("rule:".length)}`;
  return `Approved by ${by}`;
}

/** A short date such as "21 Oct 2026". */
export function formatDate(iso: string): string {
  return new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric" }).format(new Date(iso));
}
