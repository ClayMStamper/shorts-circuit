import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { MatchSuggestion, TikTokStatus } from "../lib/types";
import MatchSuggestions from "./MatchSuggestions.svelte";
import TikTokSettings from "./TikTokSettings.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

describe("TikTokSettings", () => {
  it("explains the next step when no developer app is configured", async () => {
    setInvoke(
      vi.fn(async (cmd: string) => {
        if (cmd === "tiktok_status")
          return {
            kind: "unavailable",
            reason: "No TikTok developer app is configured for this installation.",
            nextStep: "Register a TikTok developer app with the Login Kit…",
          } satisfies TikTokStatus as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(TikTokSettings, {});
    expect(await screen.findByTestId("tiktok-unavailable")).toBeTruthy();
    expect(screen.getByText(/Register a TikTok developer app/)).toBeTruthy();
    expect(screen.queryByRole("checkbox")).toBeNull();
  });

  it("is off by default and turns on through the capability flag", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "tiktok_status") return { kind: "off" } as never;
        if (cmd === "tiktok_set_enabled") return { kind: "notConnected" } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(TikTokSettings, {});
    const toggle = (await screen.findByRole("checkbox", { name: "Detect new TikTok posts" })) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    await fireEvent.click(toggle);
    expect(calls).toContainEqual(["tiktok_set_enabled", { enabled: true }]);
    expect(await screen.findByRole("button", { name: "Connect TikTok…" })).toBeTruthy();
  });
});

describe("MatchSuggestions", () => {
  const suggestion: MatchSuggestion = {
    postAssetId: "item1",
    post: {
      videoId: "7000000000000000001",
      title: "Bagels at dawn",
      caption: "Boiled, then baked",
      postedAt: "2026-10-29T18:00:00Z",
      durationMs: 20_000,
      shareUrl: null,
      coverPath: null,
      matched: false,
    },
    candidate: {
      assetId: "file1",
      title: "IMG 0001",
      originalName: "IMG_0001.mov",
      importedAt: "2026-10-30T12:00:00Z",
      durationMs: 20_000,
    },
    durationDiffMs: 0,
    hoursApart: 18,
  };

  it("merges only when the user confirms", async () => {
    const calls: Array<[string, unknown]> = [];
    const opened: string[] = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "match_suggestions") return (calls.some(([c]) => c === "confirm_match") ? [] : [suggestion]) as never;
        if (cmd === "confirm_match") return { id: "file1", title: "Bagels at dawn" } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(MatchSuggestions, { revision: 0, onOpen: (id: string) => opened.push(id) });
    expect(await screen.findByTestId("match")).toBeTruthy();
    expect(calls.some(([c]) => c === "confirm_match")).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "Yes, same video" }));
    expect(calls).toContainEqual(["confirm_match", { videoId: "7000000000000000001", assetId: "file1" }]);
    await vi.waitFor(() => expect(opened).toEqual(["file1"]));
  });
});
