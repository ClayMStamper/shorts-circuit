// Browser preview with made-up data, for designing and screenshotting the UI without the desktop
// shell: `pnpm dev`, then open http://localhost:1420/?mock (or `?mock=empty` for first run).
// Only loaded in development builds (see main.ts); nothing here talks to a real service.

import { setInvoke } from "../lib/api";
import type {
  AccountView,
  Platform,
  PublicationState,
  PublicationView,
  RuleItemView,
  Tone,
  WorkflowView,
} from "../lib/types";

const now = Date.now();
const ago = (minutes: number) => new Date(now - minutes * 60_000).toISOString();

function thumb(hue: number, label: string): string {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="90" height="160"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue},70%,55%)"/><stop offset="1" stop-color="hsl(${hue + 40},70%,30%)"/></linearGradient></defs><rect width="90" height="160" fill="url(#g)"/><text x="45" y="88" font-family="sans-serif" font-size="28" text-anchor="middle">${label}</text></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

const NAMES: Record<Platform, string> = {
  youtube_shorts: "YouTube Shorts",
  instagram_reels: "Instagram Reels",
  snapchat_spotlight: "Snapchat Spotlight",
  linkedin: "LinkedIn",
};

function account(id: string, platform: Platform, name: string, extra: Partial<AccountView> = {}): AccountView {
  return {
    id,
    platform,
    platformName: NAMES[platform],
    displayName: name,
    automatic: true,
    missingReason: null,
    nextAction: null,
    manualHandoff: true,
    connected: true,
    privateOnly: false,
    notes: [],
    ...extra,
  };
}

const accounts: AccountView[] = [
  account("yt", "youtube_shorts", "Bake Lab"),
  account("ig", "instagram_reels", "@bakelab"),
  account("li", "linkedin", "Bake Lab Ltd", {
    automatic: false,
    nextAction: "reconnect",
    missingReason: "The LinkedIn token ran out on 19 Oct.",
  }),
];

function pub(
  id: string,
  acc: AccountView,
  state: PublicationState,
  statusLabel: string,
  tone: Tone,
  extra: Partial<PublicationView> = {},
): PublicationView {
  return {
    id,
    assetId: "",
    assetTitle: "",
    platform: acc.platform,
    platformName: acc.platformName,
    accountId: acc.id,
    accountName: acc.displayName,
    state,
    revision: 1,
    variantId: "v",
    variantName: "Vertical 1080x1920",
    caption: "",
    hashtags: [],
    scheduledAt: null,
    timezone: "Europe/London",
    scheduledLocal: null,
    approvedRevision: 1,
    approved: true,
    retryAt: null,
    lastError: null,
    packageDir: null,
    publishedUrl: null,
    publishedNote: null,
    progressFraction: null,
    progressText: null,
    statusText: "",
    statusLabel,
    tone,
    approvedBy: "workflow:TikTok → " + acc.platformName,
    blockers: [],
    publishedPrivate: false,
    actions: [],
    owner: "desktop",
    cloudNote: null,
    updatedAt: ago(30),
    ...extra,
  };
}

const [yt, ig, li] = accounts as [AccountView, AccountView, AccountView];

let items: RuleItemView[] = [
  {
    assetId: "a1",
    title: "Overnight focaccia",
    caption: "Overnight focaccia, dimpled and drowned in olive oil 🫒\n#bread #baking #focaccia",
    durationMs: 34_000,
    sourcePostedAt: ago(25),
    sourceUrl: "https://www.tiktok.com/@timeless_way_interactive/video/7000000000000000001",
    waitingForMedia: false,
    waitingReason: null,
    addedAt: ago(20),
    publications: [
      pub("p1", yt, "uploading", "Processing 64%", "info", { progressFraction: 0.64, progressText: "Uploading to YouTube (64%)", actions: ["cancel"] }),
      pub("p2", ig, "published", "Published", "good", { publishedUrl: "https://www.instagram.com/reel/example/", updatedAt: ago(4), actions: ["republish"] }),
    ],
  },
  {
    assetId: "a2",
    title: "Bagels at dawn",
    caption: "Boiled, then baked. 5am bagels hit different #bagels #breakfast",
    durationMs: 21_000,
    sourcePostedAt: ago(60 * 15),
    sourceUrl: "https://www.tiktok.com/@timeless_way_interactive/video/7000000000000000002",
    waitingForMedia: false,
    waitingReason: null,
    addedAt: ago(60 * 15),
    publications: [
      pub("p3", yt, "published", "Uploaded (private)", "warn", { publishedPrivate: true, publishedUrl: "https://www.youtube.com/shorts/example", updatedAt: ago(60 * 14), actions: ["republish"] }),
      pub("p4", ig, "published", "Published", "good", { publishedUrl: "https://www.instagram.com/reel/example2/", updatedAt: ago(60 * 14), actions: ["republish"] }),
      pub("p5", li, "needs_authentication", "Needs reconnect", "bad", { lastError: "LinkedIn ended the sign-in; reconnect the page to continue.", actions: ["reconnect", "cancel"] }),
    ],
  },
  {
    assetId: "a3",
    title: "Crumb shot",
    caption: "The crumb on this sourdough 😮‍💨 full recipe on my profile, starter is 4 years old, fed twice a day with rye #sourdough #bread #crumbshot #baking",
    durationMs: 72_000,
    sourcePostedAt: ago(60 * 26),
    sourceUrl: "https://www.tiktok.com/@timeless_way_interactive/video/7000000000000000003",
    waitingForMedia: false,
    waitingReason: null,
    addedAt: ago(60 * 26),
    publications: [
      pub("p6", yt, "published", "Published", "good", { publishedUrl: "https://www.youtube.com/shorts/example3", updatedAt: ago(60 * 25), actions: ["republish"] }),
      pub("p7", ig, "awaiting_approval", "Needs attention", "warn", { approved: false, approvedBy: null, blockers: ["video is 72.0s; the maximum is 60.0s"], actions: ["review", "approve", "publishNow", "reschedule", "cancel"] }),
    ],
  },
  {
    assetId: "a4",
    title: "Lamination timelapse",
    caption: "",
    durationMs: 18_000,
    sourcePostedAt: null,
    sourceUrl: null,
    waitingForMedia: false,
    waitingReason: "Waiting for its TikTok post to use that caption (until 18:40 UTC).",
    addedAt: ago(8),
    publications: [],
  },
  {
    assetId: "a5",
    title: "Cinnamon rolls",
    caption: "Cinnamon rolls but make them tiny #baking",
    durationMs: 28_000,
    sourcePostedAt: ago(60 * 50),
    sourceUrl: "https://www.tiktok.com/@timeless_way_interactive/video/7000000000000000005",
    waitingForMedia: false,
    waitingReason: null,
    addedAt: ago(60 * 50),
    publications: [
      pub("p8", yt, "failed", "Failed", "bad", { lastError: "YouTube rejected the upload: the file is still being processed by the editor (empty video).", actions: ["retry", "edit", "cancel"] }),
      pub("p9", ig, "published", "Published", "good", { publishedUrl: "https://www.instagram.com/reel/example5/", updatedAt: ago(60 * 49), actions: ["republish"] }),
    ],
  },
];

function workflow(id: string, acc: AccountView, extra: Partial<WorkflowView> = {}): WorkflowView {
  const name = `TikTok → ${acc.platformName}`;
  return {
    id,
    name,
    ruleId: `r-${id}`,
    accountId: acc.id,
    accountName: acc.displayName,
    platform: acc.platform,
    platformName: acc.platformName,
    sourceName: "TikTok",
    enabled: true,
    autoPublish: true,
    autoPublishAvailable: true,
    status: "active",
    statusText: "Active",
    banner: `All content from TikTok on or after October 21, 2026 09:00 BST is published to ${acc.platformName} · ${acc.displayName} automatically within ~5 minutes of arriving.`,
    rightsConfirmed: true,
    posting: "asap",
    shareToFeed: null,
    captionTemplate: "{caption}",
    hashtags: [],
    effectiveFrom: "2026-10-21T08:00:00Z",
    privateOnly: false,
    itemCount: 0,
    createdAt: "2026-10-21T08:00:00Z",
    ...extra,
  };
}

let workflows: WorkflowView[] = [
  workflow("w1", yt, { itemCount: 12 }),
  workflow("w2", ig, { itemCount: 9 }),
  workflow("w3", li, {
    itemCount: 4,
    status: "needs_reconnect",
    autoPublishAvailable: false,
    statusText: "Bake Lab Ltd needs reconnecting; posts wait and continue once you reconnect.",
  }),
];

const source = { name: "TikTok", watchFolder: "C:\\Users\\clay\\OneDrive\\TikTok exports", watchCheckedAt: ago(3), tiktokConnected: true };

export function installMock(scenario: string): void {
  if (scenario === "empty") {
    workflows = [];
    items = [];
  }
  const handlers: Record<string, (args: Record<string, unknown>) => unknown> = {
    media_status: () => ({ available: true, message: null }),
    list_workflows: () => ({
      workflows,
      suggestions: accounts
        .filter((a) => a.automatic && !workflows.some((w) => w.accountId === a.id))
        .map((a) => ({ accountId: a.id, accountName: a.displayName, platform: a.platform, platformName: a.platformName, name: `TikTok → ${a.platformName}` })),
      source,
    }),
    check_for_new_content: () => handlers.list_workflows!({}),
    workflow_detail: ({ id }) => {
      const w = workflows.find((x) => x.id === id)!;
      return {
        workflow: w,
        items: items
          .map((i) => ({ ...i, publications: i.publications.filter((p) => p.accountId === w.accountId) }))
          .filter((i) => i.publications.length || i.waitingReason),
      };
    },
    update_workflow: ({ id, update }) => {
      const u = update as Partial<WorkflowView> & { autoPublish?: boolean };
      workflows = workflows.map((w) => (w.id === id ? { ...w, ...u, status: u.enabled === false ? "inactive" : w.status } : w));
      return workflows.find((w) => w.id === id);
    },
    create_workflow: ({ workflow: input }) => {
      const acc = accounts.find((a) => a.id === (input as { accountId: string }).accountId)!;
      const w = workflow(`w${workflows.length + 1}`, acc);
      workflows = [...workflows, w];
      return w;
    },
    delete_workflow: ({ id }) => {
      workflows = workflows.filter((w) => w.id !== id);
    },
    activity: () => ({ items }),
    thumbnail: ({ id }) => {
      const i = items.findIndex((x) => x.assetId === id);
      return thumb(20 + i * 60, ["🍞", "🥯", "🥖", "🥐", "🌀"][i] ?? "🎬");
    },
    list_accounts: () => accounts,
    preferences: () => ({
      timezone: "Europe/London",
      overduePolicy: { kind: "expire", grace_secs: 900 },
      watchFolder: scenario === "empty" ? null : source.watchFolder,
      exportDir: null,
      postingSlots: [],
      watchCollection: "c1",
      watchCheckedAt: source.watchCheckedAt,
      downloadsFolder: scenario === "empty" ? null : "C:\Users\clay\Downloads",
    }),
    list_collections: () => [],
    tiktok_status: () =>
      scenario === "empty"
        ? { kind: "unavailable", reason: "No TikTok developer app is set up yet.", nextStep: "Register a TikTok developer app with Login Kit (desktop)…" }
        : { kind: "connected", displayName: "Bake Lab", connectedAt: ago(60 * 48), lastCheckedAt: ago(9), lastError: null, pendingPosts: 1 },
    tiktok_app_info: () => ({ appConfigured: scenario !== "empty", clientKey: "aw1234example", brokerConfigured: false, redirectUri: "http://127.0.0.1:47616/callback/" }),
    youtube_status: () => ({
      clientConfigured: true,
      nextStep: null,
      connecting: false,
      channels: [{ accountId: "yt", displayName: "Bake Lab", connected: true, needsReconnect: false, lastError: null, privateOnly: false, notes: [], connectedAt: ago(60 * 72), signInExpiresAt: null }],
      quota: { day: "2026-10-21", uploadsUsed: 3, uploadsLimit: 100, unitsUsed: 41, unitsLimit: 10000, resetsAt: ago(-600), warning: null },
    }),
    instagram_status: () => ({
      nextStep: null,
      accounts: [{ accountId: "ig", displayName: "@bakelab", connected: true, needsReconnect: false, login: "facebook_login", canPublish: true, pageName: "Bake Lab", username: "bakelab", lastError: null, notes: [], connectedAt: ago(60 * 72) }],
    }),
    linkedin_status: () => ({ pages: [], nextStep: null }),
    snapchat_status: () => ({ clientConfigured: false, redirectUri: "http://127.0.0.1:47615/snapchat/callback", returnMode: "loopback", nextStep: "Paste your Snap OAuth app.", connecting: false, awaitingCode: false, profiles: [] }),
    cloud_status: () => ({ enabled: false, url: null, tokenSaved: false, held: 0, lastSyncAt: null, lastError: null, nextStep: null }),
  };
  setInvoke((async (cmd: string, args?: Record<string, unknown>) => {
    const handler = handlers[cmd];
    if (!handler) throw { kind: "mock", message: `${cmd} isn't available in the browser preview` };
    return handler(args ?? {});
  }) as never);
}
