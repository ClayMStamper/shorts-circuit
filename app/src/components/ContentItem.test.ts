import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { PublicationView, RuleItemView } from "../lib/types";
import ContentItem from "./ContentItem.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const published = {
  id: "p1",
  platform: "instagram_reels",
  platformName: "Instagram Reels",
  accountName: "@clay_stamper",
  state: "published",
  statusLabel: "Published",
  tone: "good",
  publishedUrl: "https://www.instagram.com/reel/AbC123/",
  publishedPrivate: false,
  progressFraction: null,
  approvedBy: null,
  blockers: [],
  actions: [],
  updatedAt: "2026-10-21T09:05:00Z",
} as unknown as PublicationView;

function item(sourceUrl: string | null): RuleItemView {
  return {
    assetId: "a1",
    title: "Mobs",
    caption: "Come vibe and make mobs #gamedevelopment",
    durationMs: 51_700,
    sourcePostedAt: "2026-10-21T08:30:00Z",
    sourceUrl,
    expectingOriginalUntil: null,
    downloadNote: null,
    waitingForMedia: false,
    waitingReason: null,
    addedAt: "2026-10-21T08:40:00Z",
    publications: [published],
  };
}

function capture() {
  const calls: Array<[string, unknown]> = [];
  setInvoke(
    vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      if (cmd === "thumbnail") return null as never;
      if (cmd === "open_link" || cmd === "cancel_get_original" || cmd === "attach_original") return undefined as never;
      if (cmd === "get_original") return { downloadsFolder: "C:\Users\clay\Downloads", enabledNow: true } as never;
      throw new Error(`unexpected ${cmd}`);
    }) as never,
  );
  return calls;
}

describe("ContentItem links", () => {
  it("opens the original TikTok post and the published post", async () => {
    const calls = capture();
    render(ContentItem, { item: item("https://www.tiktok.com/@timeless_way_interactive/video/7569928341285424398") });
    await fireEvent.click(screen.getByTestId("source-link"));
    expect(screen.getByTestId("source-link").textContent).toContain("Open on TikTok");
    await fireEvent.click(screen.getByTestId("published-link"));
    expect(calls).toContainEqual(["open_link", { url: "https://www.tiktok.com/@timeless_way_interactive/video/7569928341285424398" }]);
    expect(calls).toContainEqual(["open_link", { url: "https://www.instagram.com/reel/AbC123/" }]);
  });

  it("shows no source link when the original post isn't known", () => {
    capture();
    render(ContentItem, { item: item(null) });
    expect(screen.queryByTestId("source-link")).toBeNull();
    expect(screen.getByTestId("published-link")).toBeTruthy();
  });
});

describe("Get original", () => {
  it("opens the waiting post and expects its download", async () => {
    const calls = capture();
    const waiting: RuleItemView = {
      ...item("https://www.tiktok.com/@timeless_way_interactive/video/7569928341285424398"),
      waitingForMedia: true,
      publications: [],
    };
    render(ContentItem, { item: waiting });
    await fireEvent.click(screen.getByTestId("get-original"));
    expect(calls).toContainEqual(["get_original", { assetId: "a1" }]);
  });

  it("isn't offered without a link to the post", () => {
    capture();
    render(ContentItem, { item: { ...item(null), waitingForMedia: true, publications: [] } });
    expect(screen.queryByTestId("get-original")).toBeNull();
    expect(screen.getByText(/Save it to your watch folder/)).toBeTruthy();
  });
});

describe("Waiting for a download", () => {
  it("shows the wait, the note, and lets the owner stop waiting", async () => {
    const calls = capture();
    render(ContentItem, {
      item: {
        ...item("https://www.tiktok.com/@timeless_way_interactive/video/7"),
        waitingForMedia: true,
        publications: [],
        expectingOriginalUntil: "2026-10-21T21:58:00Z",
        downloadNote: "Saw “Download (3).mp4” in your downloads, but it's 40 s and this post is 25 s.",
      },
    });
    expect(screen.getByText("Waiting for your download")).toBeTruthy();
    expect(screen.getByTestId("download-note").textContent).toContain("40 s");
    expect(screen.queryByTestId("get-original")).toBeNull();
    await fireEvent.click(screen.getByTestId("cancel-original"));
    expect(calls).toContainEqual(["cancel_get_original", { assetId: "a1" }]);
  });
});
