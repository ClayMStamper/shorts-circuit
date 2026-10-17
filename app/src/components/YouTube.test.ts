import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { MonitorView, PublicationView, YouTubeStatus } from "../lib/types";
import Monitor from "./Monitor.svelte";
import YouTubeSettings from "./YouTubeSettings.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const quota = {
  day: "2026-10-30",
  uploadsUsed: 0,
  uploadsLimit: 100,
  unitsUsed: 0,
  unitsLimit: 10_000,
  resetsAt: "2026-10-31T07:00:00Z",
  warning: null,
};

const unconfigured: YouTubeStatus = {
  clientConfigured: false,
  nextStep: "Create a Google Cloud project with the YouTube Data API v3 enabled…",
  connecting: false,
  channels: [],
  quota,
};

const connected: YouTubeStatus = {
  clientConfigured: true,
  nextStep: null,
  connecting: false,
  channels: [
    {
      accountId: "acc1",
      displayName: "Test Kitchen",
      connected: true,
      needsReconnect: false,
      lastError: null,
      privateOnly: true,
      notes: ["Quota: 100 uploads and 10000 units a day…"],
      connectedAt: "2026-10-30T12:00:00Z",
      signInExpiresAt: "2026-11-06T12:00:00Z",
    },
  ],
  quota: { ...quota, uploadsUsed: 85, warning: "Today's YouTube API quota is running low" },
};

describe("YouTubeSettings", () => {
  it("saves the OAuth client and never shows the secret again", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "youtube_status") return unconfigured as never;
        if (cmd === "youtube_set_client") return { ...unconfigured, clientConfigured: true, nextStep: null } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(YouTubeSettings, {});
    const id = (await screen.findByLabelText("Client ID")) as HTMLInputElement;
    const secret = screen.getByLabelText("Client secret") as HTMLInputElement;
    expect(secret.type).toBe("password");
    expect((screen.getByRole("button", { name: "Connect a YouTube channel…" }) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(id, { target: { value: "123-x.apps.googleusercontent.com" } });
    await fireEvent.input(secret, { target: { value: "GOCSPX-secret" } });
    await fireEvent.submit(screen.getByRole("form", { name: "Google OAuth client" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "youtube_set_client",
        { clientId: "123-x.apps.googleusercontent.com", clientSecret: "GOCSPX-secret" },
      ]),
    );
    expect(await screen.findByTestId("client-saved")).toBeTruthy();
    expect(document.body.innerHTML).not.toContain("GOCSPX-secret");
  });

  it("explains private-until-audit uploads and warns about quota", async () => {
    setInvoke(
      vi.fn(async (cmd: string) => {
        if (cmd === "youtube_status") return connected as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(YouTubeSettings, {});
    expect((await screen.findByTestId("private-note")).textContent).toContain("audit");
    expect(screen.getByTestId("quota-warning").textContent).toContain("running low");
    expect(screen.getByText(/Uploads: 85 of 100/)).toBeTruthy();
    const toggle = screen.getByRole("checkbox", { name: /Upload as public/ }) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
  });

  it("offers Reconnect when Google ended the sign-in", async () => {
    const calls: string[] = [];
    const expired: YouTubeStatus = {
      ...connected,
      channels: [{ ...connected.channels[0]!, needsReconnect: true, lastError: "Google ended the channel's sign-in" }],
    };
    setInvoke(
      vi.fn(async (cmd: string) => {
        calls.push(cmd);
        if (cmd === "youtube_status") return expired as never;
        if (cmd === "youtube_connect") return undefined as never;
        if (cmd === "youtube_connect_progress") return { kind: "pending" } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(YouTubeSettings, {});
    await fireEvent.click(await screen.findByRole("button", { name: "Reconnect Test Kitchen…" }));
    expect(calls).toContain("youtube_connect");
  });
});

describe("Monitor", () => {
  const base: PublicationView = {
    id: "p1",
    assetId: "a1",
    assetTitle: "Bagels",
    platform: "youtube_shorts",
    platformName: "YouTube Shorts",
    accountId: "acc1",
    accountName: "Test Kitchen",
    state: "published",
    revision: 1,
    variantId: "v1",
    variantName: "Vertical",
    caption: "Bagels",
    hashtags: [],
    scheduledAt: null,
    timezone: null,
    scheduledLocal: null,
    approvedRevision: 1,
    approved: true,
    retryAt: null,
    lastError: null,
    packageDir: null,
    publishedUrl: "https://www.youtube.com/shorts/vid1",
    publishedNote: "Uploaded as private (this channel is set to private uploads…)",
    progressFraction: null,
    progressText: null,
    statusText: "Published: https://www.youtube.com/shorts/vid1",
    actions: ["exportPackage"],
    updatedAt: "2026-10-30T12:00:00Z",
  };

  function monitor(publications: PublicationView[]) {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "monitor") return { publications, jobs: [] } satisfies MonitorView as never;
        if (cmd === "open_link") return undefined as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    return calls;
  }

  it("links the published video and shows its visibility note", async () => {
    const calls = monitor([base]);
    render(Monitor, { revision: 0, onReview: () => {}, onOpenAsset: () => {} });
    expect((await screen.findByTestId("published-note")).textContent).toContain("private");
    await fireEvent.click(screen.getByRole("button", { name: "Open on YouTube Shorts" }));
    expect(calls).toContainEqual(["open_link", { url: "https://www.youtube.com/shorts/vid1" }]);
  });

  it("shows processing progress and sends Reconnect to Settings", async () => {
    monitor([
      { ...base, id: "p2", state: "processing", publishedUrl: null, publishedNote: null, progressText: "YouTube is processing the video (50%)", actions: ["cancel"] },
      { ...base, id: "p3", state: "needs_authentication", publishedUrl: null, publishedNote: null, actions: ["reconnect", "edit", "cancel"] },
    ]);
    const onSettings = vi.fn();
    render(Monitor, { revision: 0, onReview: () => {}, onOpenAsset: () => {}, onSettings });
    expect((await screen.findByTestId("progress")).textContent).toContain("50%");
    await fireEvent.click(screen.getByRole("button", { name: "Reconnect…" }));
    expect(onSettings).toHaveBeenCalled();
  });
});
