import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { CloudStatus, MonitorView, PublicationView } from "../lib/types";
import CloudSettings from "./CloudSettings.svelte";
import Monitor from "./Monitor.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const off: CloudStatus = {
  enabled: false,
  url: null,
  tokenSaved: false,
  held: 0,
  lastSyncAt: null,
  lastError: null,
  nextStep: "Set up a cloud runner…",
};

describe("CloudSettings", () => {
  it("is off by default and connects without echoing the token", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "cloud_status") return off as never;
        if (cmd === "cloud_configure")
          return { ...off, enabled: true, url: "https://runner.example.com/", tokenSaved: true, nextStep: null } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(CloudSettings, {});
    expect(await screen.findByTestId("cloud-next-step")).toBeTruthy();
    expect(screen.queryByTestId("cloud-on")).toBeNull();
    await fireEvent.input(screen.getByLabelText("Runner address"), { target: { value: "https://runner.example.com" } });
    const field = screen.getByLabelText("Installation token") as HTMLInputElement;
    expect(field.type).toBe("password");
    await fireEvent.input(field, { target: { value: "sc_secret-token" } });
    await fireEvent.submit(screen.getByRole("form", { name: "Cloud runner" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual(["cloud_configure", { url: "https://runner.example.com", token: "sc_secret-token" }]),
    );
    expect(await screen.findByTestId("cloud-on")).toBeTruthy();
    expect(document.body.innerHTML).not.toContain("sc_secret-token");
  });

  it("can't be turned off while the runner holds publications", async () => {
    setInvoke(
      vi.fn(async (cmd: string) => {
        if (cmd === "cloud_status")
          return { ...off, enabled: true, url: "https://runner.example.com/", tokenSaved: true, held: 2, nextStep: null } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(CloudSettings, {});
    const button = (await screen.findByRole("button", { name: "Turn off" })) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(screen.getByTestId("cloud-on").textContent).toContain("2 publications");
  });
});

const publication: PublicationView = {
  id: "p1",
  assetId: "a1",
  assetTitle: "Morning routine",
  platform: "youtube_shorts",
  platformName: "YouTube Shorts",
  accountId: "acc1",
  accountName: "Channel",
  state: "scheduled",
  revision: 1,
  variantId: "v1",
  variantName: "Vertical",
  caption: "Hello",
  hashtags: [],
  scheduledAt: "2026-11-02T09:00:00Z",
  timezone: "UTC",
  scheduledLocal: "Mon 2026-11-02 09:00 UTC",
  approvedRevision: 1,
  approved: true,
  retryAt: null,
  lastError: null,
  packageDir: null,
  publishedUrl: null,
  publishedNote: null,
  progressFraction: null,
  progressText: null,
  statusText: "The cloud runner posts it Mon 2026-11-02 09:00 UTC, even if this computer is off.",
  actions: ["takeBack"],
  owner: "cloud",
  cloudNote: "Not on the cloud runner yet: could not connect",
  updatedAt: "2026-10-30T12:00:00Z",
  statusLabel: "Scheduled",
  tone: "info",
  approvedBy: null,
  blockers: [],
  publishedPrivate: false,
};

describe("Monitor in cloud mode", () => {
  it("marks cloud-owned publications and takes them back", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "monitor") return { publications: [publication], jobs: [] } satisfies MonitorView as never;
        if (cmd === "take_back_from_cloud") return { ...publication, owner: "desktop", actions: [] } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(Monitor, { revision: 0, onReview: () => {}, onOpenAsset: () => {} });
    expect(await screen.findByTestId("cloud-owned")).toBeTruthy();
    expect(screen.getByTestId("cloud-note").textContent).toContain("could not connect");
    await fireEvent.click(screen.getByRole("button", { name: "Take back" }));
    await vi.waitFor(() => expect(calls).toContainEqual(["take_back_from_cloud", { id: "p1" }]));
  });

  it("offers posting from the cloud for approved publications", async () => {
    const calls: Array<[string, unknown]> = [];
    const local = { ...publication, owner: "desktop" as const, cloudNote: null, actions: ["review", "sendToCloud"] as PublicationView["actions"] };
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "monitor") return { publications: [local], jobs: [] } satisfies MonitorView as never;
        if (cmd === "send_to_cloud") return publication as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(Monitor, { revision: 0, onReview: () => {}, onOpenAsset: () => {} });
    await fireEvent.click(await screen.findByRole("button", { name: "Post from the cloud" }));
    await vi.waitFor(() => expect(calls).toContainEqual(["send_to_cloud", { id: "p1" }]));
  });
});
