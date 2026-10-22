import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { PublicationView, RuleItemView, WorkflowView, WorkflowsView } from "../lib/types";
import Workflows from "./Workflows.svelte";
import WorkflowDetail from "./WorkflowDetail.svelte";

function workflow(overrides: Partial<WorkflowView> = {}): WorkflowView {
  return {
    id: "w1",
    name: "TikTok → YouTube Shorts",
    ruleId: "r1",
    accountId: "yt",
    accountName: "Bake Lab",
    platform: "youtube_shorts",
    platformName: "YouTube Shorts",
    sourceName: "TikTok",
    enabled: true,
    autoPublish: true,
    autoPublishAvailable: true,
    status: "active",
    statusText: "Active",
    banner:
      "All content from TikTok on or after October 21, 2026 09:00 BST is published to YouTube Shorts · Bake Lab automatically within ~5 minutes of arriving.",
    rightsConfirmed: true,
    posting: "asap",
    shareToFeed: null,
    captionTemplate: "{caption}",
    hashtags: [],
    effectiveFrom: "2026-10-21T08:00:00Z",
    privateOnly: false,
    itemCount: 3,
    createdAt: "2026-10-21T08:00:00Z",
    ...overrides,
  };
}

const source = { name: "TikTok", watchFolder: "C:\\Videos", watchCheckedAt: "2026-10-21T09:00:00Z", tiktokConnected: false };

function list(workflows: WorkflowView[], suggestions: WorkflowsView["suggestions"] = []): WorkflowsView {
  return { workflows, suggestions, source };
}

/** Answers the commands every connection card asks for, plus `extra`. */
function fake(extra: Record<string, (args: Record<string, unknown>) => unknown>) {
  const calls: Array<[string, Record<string, unknown> | undefined]> = [];
  const defaults: Record<string, (args: Record<string, unknown>) => unknown> = {
    list_accounts: () => [],
    preferences: () => ({
      timezone: "Europe/London",
      overduePolicy: { kind: "catch_up" },
      watchFolder: null,
      exportDir: null,
      postingSlots: [],
      watchCollection: null,
      watchCheckedAt: null,
      downloadsFolder: null,
    }),
    tiktok_status: () => ({ kind: "off" }),
    tiktok_app_info: () => ({ appConfigured: false, clientKey: null, brokerConfigured: false, redirectUri: "http://127.0.0.1:47616/callback/" }),
    youtube_status: () => ({
      clientConfigured: false,
      nextStep: null,
      connecting: false,
      channels: [],
      quota: { day: "2026-10-21", uploadsUsed: 0, uploadsLimit: 100, unitsUsed: 0, unitsLimit: 10000, resetsAt: "2026-10-22T07:00:00Z", warning: null },
    }),
    instagram_status: () => ({ accounts: [], nextStep: null }),
    linkedin_status: () => ({ pages: [], nextStep: null }),
    snapchat_status: () => ({
      clientConfigured: false,
      redirectUri: "http://127.0.0.1:47615/snapchat/callback",
      returnMode: "loopback",
      nextStep: null,
      connecting: false,
      awaitingCode: false,
      profiles: [],
    }),
    thumbnail: () => null,
  };
  const handlers = { ...defaults, ...extra };
  setInvoke(
    vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      const h = handlers[cmd];
      if (!h) throw new Error(`unexpected ${cmd}`);
      return h(args ?? {}) as never;
    }) as never,
  );
  return calls;
}

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

describe("Workflows page", () => {
  it("lists source → destination rows with Auto Publish, status, count and actions", async () => {
    fake({
      list_workflows: () =>
        list([
          workflow(),
          workflow({ id: "w2", name: "TikTok → LinkedIn", platform: "linkedin", platformName: "LinkedIn", accountName: "Page", status: "needs_reconnect", itemCount: 1 }),
          workflow({ id: "w3", name: "TikTok → Instagram Reels", platform: "instagram_reels", platformName: "Instagram Reels", enabled: false, status: "inactive", itemCount: 0 }),
        ]),
    });
    render(Workflows, { onOpen: vi.fn(), onConnections: vi.fn() });
    const rows = await screen.findAllByTestId("workflow-row");
    expect(rows).toHaveLength(3);
    const yt = within(rows[0]!);
    expect(yt.getByRole("button", { name: "TikTok → YouTube Shorts" })).toBeTruthy();
    expect((yt.getByRole("switch", { name: "Auto Publish" }) as HTMLElement).getAttribute("aria-checked")).toBe("true");
    expect(yt.getByText("Active")).toBeTruthy();
    expect(yt.getByText("3 items")).toBeTruthy();
    expect(within(rows[1]!).getByText("Needs reconnect")).toBeTruthy();
    expect(within(rows[1]!).getByRole("button", { name: "Reconnect" })).toBeTruthy();
    expect(within(rows[2]!).getByRole("button", { name: "Reactivate" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Create workflow" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Check for new content now" })).toBeTruthy();
    expect(screen.getByText(/Last checked/)).toBeTruthy();
  });

  it("turns Auto Publish off from the row, and reactivates a paused workflow", async () => {
    let current = [workflow(), workflow({ id: "w3", enabled: false, status: "inactive", name: "Paused one" })];
    const calls = fake({
      list_workflows: () => list(current),
      update_workflow: ({ id, update }) => {
        const u = update as Partial<WorkflowView>;
        current = current.map((w) => (w.id === id ? { ...w, ...u } : w));
        return current.find((w) => w.id === id);
      },
    });
    render(Workflows, { onOpen: vi.fn(), onConnections: vi.fn() });
    const rows = await screen.findAllByTestId("workflow-row");
    await fireEvent.click(within(rows[0]!).getByRole("switch", { name: "Auto Publish" }));
    expect(calls).toContainEqual(["update_workflow", { id: "w1", update: { autoPublish: false } }]);
    await vi.waitFor(() =>
      expect(within(rows[0]!).getByRole("switch", { name: "Auto Publish" }).getAttribute("aria-checked")).toBe("false"),
    );
    await fireEvent.click(within(rows[1]!).getByRole("button", { name: "Reactivate" }));
    expect(calls).toContainEqual(["update_workflow", { id: "w3", update: { enabled: true } }]);
  });

  it("can't turn Auto Publish on for an account that isn't connected", async () => {
    fake({ list_workflows: () => list([workflow({ autoPublish: false, autoPublishAvailable: false })]) });
    render(Workflows, { onOpen: vi.fn(), onConnections: vi.fn() });
    const toggle = await screen.findByRole("switch", { name: "Auto Publish" });
    expect((toggle as HTMLButtonElement).disabled).toBe(true);
  });

  it("opens the overflow menu with rename, pause, advanced and delete", async () => {
    const onOpen = vi.fn();
    fake({ list_workflows: () => list([workflow()]) });
    render(Workflows, { onOpen, onConnections: vi.fn() });
    await fireEvent.click(await screen.findByRole("button", { name: "More for TikTok → YouTube Shorts" }));
    const menu = screen.getByRole("menu");
    const items = within(menu).getAllByRole("menuitem").map((b) => b.textContent);
    expect(items).toEqual(["Rename", "Pause", "Advanced settings", "Delete"]);
    await fireEvent.click(within(menu).getByRole("menuitem", { name: "Advanced settings" }));
    expect(onOpen).toHaveBeenCalledWith("w1", true);
  });
});

describe("Setup wizard", () => {
  it("shows until a workflow exists and creates one per connected destination with the rights checkbox", async () => {
    let created = false;
    const calls = fake({
      list_workflows: () =>
        created
          ? list([workflow()])
          : list([], [{ accountId: "yt", accountName: "Bake Lab", platform: "youtube_shorts", platformName: "YouTube Shorts", name: "TikTok → YouTube Shorts" }]),
      create_workflow: () => {
        created = true;
        return workflow();
      },
    });
    render(Workflows, { onOpen: vi.fn(), onConnections: vi.fn() });
    expect(await screen.findByRole("heading", { name: "Set up Shorts Circuit" })).toBeTruthy();
    // 1. Source explains why the folder is needed.
    expect(screen.getByText(/TikTok's API can't hand over your video files/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Choose folder…" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Next: destinations" }));
    // 2. One guided card per platform, each with a numbered checklist and a status.
    const cards = await screen.findAllByTestId("destination-card");
    expect(cards).toHaveLength(4);
    expect(within(cards[0]!).getByText("Not connected")).toBeTruthy();
    expect(within(cards[0]!).getByRole("button", { name: /Google Cloud console/ })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Next: workflows" }));
    // 3. Suggested workflows, Auto Publish on, one rights checkbox.
    const pick = (await screen.findByRole("checkbox", { name: "Create TikTok → YouTube Shorts" })) as HTMLInputElement;
    expect(pick.checked).toBe(true);
    expect(screen.getByRole("switch", { name: "Auto Publish" }).getAttribute("aria-checked")).toBe("true");
    await fireEvent.click(screen.getByRole("checkbox", { name: "I own or have the rights to content from this source" }));
    await fireEvent.click(screen.getByRole("button", { name: "Create 1 workflow" }));
    expect(calls).toContainEqual(["create_workflow", { workflow: { accountId: "yt", autoPublish: true, rightsConfirmed: true } }]);
    expect(await screen.findAllByTestId("workflow-row")).toHaveLength(1);
  });
});

function pub(overrides: Partial<PublicationView>): PublicationView {
  return {
    id: "p1",
    assetId: "a1",
    assetTitle: "Bagels",
    platform: "youtube_shorts",
    platformName: "YouTube Shorts",
    accountId: "yt",
    accountName: "Bake Lab",
    state: "published",
    revision: 1,
    variantId: "v1",
    variantName: "Vertical",
    caption: "Bagels",
    hashtags: [],
    scheduledAt: "2026-10-21T09:00:00Z",
    timezone: "Europe/London",
    scheduledLocal: null,
    approvedRevision: 1,
    approved: true,
    retryAt: null,
    lastError: null,
    packageDir: null,
    publishedUrl: "https://www.youtube.com/shorts/x",
    publishedNote: null,
    progressFraction: null,
    progressText: null,
    statusText: "",
    statusLabel: "Published",
    tone: "good",
    approvedBy: "workflow:TikTok → YouTube Shorts",
    blockers: [],
    publishedPrivate: false,
    actions: ["republish"],
    owner: "desktop",
    cloudNote: null,
    updatedAt: "2026-10-21T09:05:00Z",
    ...overrides,
  };
}

function item(id: string, caption: string, publications: PublicationView[], durationMs = 20_000): RuleItemView {
  return {
    assetId: id,
    title: caption,
    caption,
    durationMs,
    sourcePostedAt: "2026-10-21T08:30:00Z",
    sourceUrl: null,
    expectingOriginalUntil: null,
    downloadNote: null,
    waitingForMedia: false,
    waitingReason: null,
    addedAt: "2026-10-21T08:40:00Z",
    publications,
  };
}

describe("Workflow detail", () => {
  const items = [
    item("a1", "Bagels at dawn #bagels", [pub({})]),
    item("a2", "Private upload", [
      pub({ id: "p2", statusLabel: "Uploaded (private)", tone: "warn", publishedPrivate: true }),
    ]),
    item(
      "a3",
      "Too long for Shorts",
      [
        pub({
          id: "p3",
          state: "awaiting_approval",
          statusLabel: "Waiting for review",
          tone: "warn",
          approved: false,
          approvedBy: null,
          scheduledAt: "2026-10-21T08:00:00Z",
          actions: ["review", "approve", "publishNow", "reschedule", "cancel"],
        }),
      ],
      200_000,
    ),
    item("a4", "Broke", [pub({ id: "p4", state: "failed", statusLabel: "Failed", tone: "bad", lastError: "YouTube said no", actions: ["retry", "cancel"] })]),
  ];

  it("shows source → destination, the plain-language banner and every item's status and actions", async () => {
    const calls = fake({
      workflow_detail: () => ({ workflow: workflow(), items }),
      list_workflows: () => list([workflow()]),
      publish_now: () => pub({ id: "p3", state: "scheduled" }),
      retry_publication: () => pub({ id: "p4", state: "scheduled" }),
    });
    render(WorkflowDetail, { id: "w1", onBack: vi.fn(), onConnections: vi.fn() });
    expect(await screen.findByText(/automatically within ~5 minutes/)).toBeTruthy();
    expect(screen.getByRole("heading", { name: /＠TikTok.*＠Bake Lab/ })).toBeTruthy();
    expect(screen.getByText(/Last checked/)).toBeTruthy();
    expect(screen.getAllByTestId("content-item")).toHaveLength(4);
    expect(screen.getByText("Uploaded (private)")).toBeTruthy();
    expect(screen.getByText(/make it public in YouTube Studio/)).toBeTruthy();
    expect(screen.getAllByText("Approved by workflow TikTok → YouTube Shorts").length).toBeGreaterThan(0);
    expect(screen.getByText("YouTube said no")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(calls).toContainEqual(["publish_now", { id: "p3" }]);
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(calls).toContainEqual(["retry_publication", { id: "p4" }]);
  });

  it("filters by status and duration", async () => {
    fake({
      workflow_detail: () => ({ workflow: workflow(), items }),
      list_workflows: () => list([workflow()]),
    });
    render(WorkflowDetail, { id: "w1", onBack: vi.fn(), onConnections: vi.fn() });
    await screen.findAllByTestId("content-item");
    await fireEvent.change(screen.getByLabelText("Status"), { target: { value: "waiting" } });
    expect(screen.getAllByTestId("content-item")).toHaveLength(1);
    await fireEvent.change(screen.getByLabelText("Status"), { target: { value: "all" } });
    await fireEvent.change(screen.getByLabelText("Duration"), { target: { value: "long" } });
    expect(screen.getAllByTestId("content-item")).toHaveLength(1);
    expect(screen.getByText("Too long for Shorts")).toBeTruthy();
  });

  it("says when review is required, and the settings drawer turns review on", async () => {
    const calls = fake({
      workflow_detail: () => ({ workflow: workflow(), items: [] }),
      list_workflows: () => list([workflow()]),
      update_workflow: ({ update }) => workflow({ ...(update as Partial<WorkflowView>) }),
    });
    render(WorkflowDetail, { id: "w1", onBack: vi.fn(), onConnections: vi.fn() });
    expect(await screen.findByText("Nothing here yet")).toBeTruthy();
    expect(screen.getByText(/goes out automatically/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const dialog = screen.getByRole("dialog", { name: "Workflow settings" });
    const review = within(dialog).getByRole("checkbox", { name: "Require review" }) as HTMLInputElement;
    expect(review.checked).toBe(false);
    expect((within(dialog).getByLabelText("Caption") as HTMLTextAreaElement).value).toBe("{caption}");
    await fireEvent.click(review);
    await fireEvent.input(within(dialog).getByLabelText("Extra hashtags"), { target: { value: "#shorts, recipes" } });
    await fireEvent.submit(within(dialog).getByRole("form", { name: "Workflow settings" }));
    expect(calls).toContainEqual(["update_workflow", { id: "w1", update: { autoPublish: false, hashtags: ["shorts", "recipes"] } }]);
  });

  it("toggles Auto Publish from the header", async () => {
    const calls = fake({
      workflow_detail: () => ({ workflow: workflow({ autoPublish: false, banner: "… waits for your review." }), items: [] }),
      list_workflows: () => list([workflow()]),
      update_workflow: () => workflow(),
    });
    render(WorkflowDetail, { id: "w1", onBack: vi.fn(), onConnections: vi.fn() });
    expect((await screen.findAllByText(/waits for your review/)).length).toBeGreaterThan(0);
    await fireEvent.click(screen.getByRole("switch", { name: "Auto Publish" }));
    expect(calls).toContainEqual(["update_workflow", { id: "w1", update: { autoPublish: true } }]);
  });
});
