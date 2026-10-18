import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import { defaultRecipe } from "../lib/recipe";
import type { ReviewView } from "../lib/types";
import Review from "./Review.svelte";

function review(overrides: Partial<ReviewView> = {}): ReviewView {
  const props = {
    duration_ms: 20_000,
    width: 1080,
    height: 1920,
    rotation: 0,
    frame_rate: 30,
    video_codec: "h264",
    audio_codec: "aac",
    audio_streams: 1,
    container: "mp4",
    bit_rate: null,
  };
  return {
    publication: {
      id: "p1",
      assetId: "a1",
      assetTitle: "Sourdough",
      platform: "instagram_reels",
      platformName: "Instagram Reels",
      accountId: "acc1",
      accountName: "Bakery",
      state: "awaiting_approval",
      revision: 2,
      variantId: "v1",
      variantName: "Vertical",
      caption: "Crispy",
      hashtags: ["bread"],
      scheduledAt: "2026-11-02T14:00:00Z",
      timezone: "America/New_York",
      scheduledLocal: "Mon 2026-11-02 09:00 EST",
      approvedRevision: null,
      approved: false,
      retryAt: null,
      lastError: null,
      packageDir: null,
      publishedUrl: null,
      publishedNote: null,
      progressFraction: null,
      progressText: null,
      statusText: "Waiting for your approval.",
      actions: ["review", "approve", "edit", "reschedule", "exportPackage", "cancel"],
      owner: "desktop",
      cloudNote: null,
      updatedAt: "2026-10-30T12:00:00Z",
    },
    account: {
      id: "acc1",
      platform: "instagram_reels",
      platformName: "Instagram Reels",
      displayName: "Bakery",
      automatic: false,
      missingReason: "No platform API connection is configured for this account.",
      nextAction: "export_package",
      manualHandoff: true,
      connected: false,
      privateOnly: false,
      notes: [],
    },
    variant: {
      id: "v1",
      assetId: "a1",
      name: "Vertical",
      revision: 1,
      recipe: defaultRecipe(),
      rendered: { path: "/cache/r.mp4", sizeBytes: 1000, properties: props, renderedAt: "2026-10-30T12:00:00Z" },
      rendering: false,
      outputDurationMs: 20_000,
    },
    videoPath: "/cache/r.mp4",
    fullCaption: "Crispy\n\n#bread",
    issues: [],
    issueText: [],
    warnings: [],
    canApprove: true,
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

describe("Review", () => {
  it("shows video, destination, copy and time together and approves the shown revision", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "review") return review() as never;
        if (cmd === "approve") return review().publication as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(Review, { id: "p1", revision: 0, onBack: () => {} });

    expect(await screen.findByTestId("review-video")).toBeTruthy();
    expect(screen.getByText("Bakery", { exact: false })).toBeTruthy();
    expect(screen.getByTestId("caption").textContent).toBe("Crispy\n\n#bread");
    expect(screen.getByTestId("time").textContent).toMatch(/09:00/);

    const approve = screen.getByRole("button", { name: "Approve revision 2" });
    await fireEvent.click(approve);
    expect(calls).toContainEqual(["approve", { id: "p1", revision: 2 }]);
  });

  it("blocks approval until the video is rendered", async () => {
    setInvoke(
      vi.fn(async (cmd: string) => {
        if (cmd === "review")
          return review({
            videoPath: null,
            canApprove: false,
            issueText: ["the variant has not been rendered yet"],
          }) as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(Review, { id: "p1", revision: 0, onBack: () => {} });
    const approve = await screen.findByRole("button", { name: "Approve revision 2" });
    expect((approve as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("the variant has not been rendered yet")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Render now" })).toBeTruthy();
  });
});
