<script lang="ts" module>
  import type { Platform } from "../lib/types";

  interface Step {
    text: string;
    link?: { label: string; url: string };
  }

  // The owner's one-time steps per platform (details in docs/human-scope.md).
  const STEPS: Record<Platform, Step[]> = {
    youtube_shorts: [
      { text: "Create a Google Cloud project with the Google account that owns the channel.", link: { label: "Google Cloud console", url: "https://console.cloud.google.com/projectcreate" } },
      { text: "Enable the YouTube Data API v3.", link: { label: "API library", url: "https://console.cloud.google.com/apis/library/youtube.googleapis.com" } },
      { text: "Consent screen: External, Testing; add your Google account as a test user.", link: { label: "Google Auth Platform", url: "https://console.cloud.google.com/auth/overview" } },
      { text: "Create an OAuth client of type “Desktop app” and paste its ID and secret below.", link: { label: "Clients", url: "https://console.cloud.google.com/auth/clients" } },
      { text: "Connect your channel below." },
    ],
    instagram_reels: [
      { text: "In Instagram, switch to a Creator or Business account (Settings → Account type and tools)." },
      { text: "Link it to your Facebook Page: on the Page, Settings → Linked accounts → Instagram → Connect.", link: { label: "Facebook Pages", url: "https://www.facebook.com/pages/?category=your_pages" } },
      { text: "In your Meta app, choose Instagram → “API setup with Facebook login”.", link: { label: "Meta for Developers", url: "https://developers.facebook.com/apps/" } },
      { text: "Graph API Explorer: pick your app, add instagram_basic, instagram_content_publish, pages_show_list, pages_read_engagement and business_management, then Generate Access Token and allow your Page.", link: { label: "Graph API Explorer", url: "https://developers.facebook.com/tools/explorer/" } },
      { text: "Access Token Debugger: paste that token, click Extend Access Token, copy the long-lived token.", link: { label: "Access Token Debugger", url: "https://developers.facebook.com/tools/debug/accesstoken/" } },
      { text: "Paste it below, click Find my Pages and pick your Page." },
    ],
    linkedin: [
      { text: "Create a LinkedIn app linked to your Company Page.", link: { label: "LinkedIn Developers", url: "https://www.linkedin.com/developers/apps/new" } },
      { text: "Products → request the Community Management API and wait for approval." },
      { text: "Generate a token with w_organization_social, r_organization_social and rw_organization_admin.", link: { label: "Token generator", url: "https://www.linkedin.com/developers/tools/oauth/token-generator" } },
      { text: "Paste it below and pick your page." },
    ],
    snapchat_spotlight: [
      { text: "Make sure the Snapchat account has a Public Profile." },
      { text: "Ads Manager → Business Details → OAuth apps: create an app and copy its client ID and secret.", link: { label: "Snapchat Ads Manager", url: "https://ads.snapchat.com/" } },
      { text: "Add the redirect URI http://127.0.0.1:47615/snapchat/callback to that app." },
      { text: "Ask Snap to allowlist the client ID for the Public Profile API (the slow part)." },
      { text: "Paste the app below, then connect your profile." },
    ],
  };
</script>

<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { platformLabel } from "../lib/format";
  import type { AccountView } from "../lib/types";
  import InstagramSettings from "./InstagramSettings.svelte";
  import LinkedInSettings from "./LinkedInSettings.svelte";
  import SnapchatSettings from "./SnapchatSettings.svelte";
  import YouTubeSettings from "./YouTubeSettings.svelte";
  import Pill from "./ui/Pill.svelte";
  import PlatformIcon from "./ui/PlatformIcon.svelte";

  let {
    platform,
    revision = 0,
    onChanged,
  }: { platform: Platform; revision?: number; onChanged?: () => void } = $props();

  let accounts = $state<AccountView[]>([]);
  let open = $state(false);

  async function load() {
    try {
      accounts = (await api.listAccounts()).filter((a) => a.platform === platform);
    } catch {
      accounts = [];
    }
  }

  onMount(load);
  $effect(() => {
    void revision;
    load();
  });

  const connected = $derived(accounts.filter((a) => a.automatic));
  const reconnect = $derived(accounts.filter((a) => a.nextAction === "reconnect"));
  const status = $derived(
    reconnect.length
      ? { tone: "bad" as const, label: "Needs reconnect" }
      : connected.length
        ? { tone: "good" as const, label: "Connected" }
        : { tone: "muted" as const, label: "Not connected" },
  );
  // One next step, whatever state the connection is in.
  const next = $derived(
    reconnect.length
      ? platform === "instagram_reels" && reconnect[0]?.missingReason?.includes("Instagram login")
        ? `Reconnect ${reconnect[0]?.displayName} with Facebook login: Instagram login can't upload files, so its videos become export packages until you do.`
        : `Reconnect ${reconnect[0]?.displayName}: its sign-in ended. Posts wait and continue once you do.`
      : connected.length
        ? null
        : "Follow the steps, then connect below.",
  );

  async function changed() {
    await load();
    onChanged?.();
  }
</script>

<article class="box conn-card" data-testid="destination-card" aria-labelledby="dest-{platform}">
  <header>
    <PlatformIcon {platform} />
    <h3 id="dest-{platform}">{platformLabel(platform)}</h3>
    <Pill tone={status.tone} label={status.label} />
  </header>
  {#if connected.length}
    <p>
      {#each connected as a, i (a.id)}{i ? ", " : ""}<strong>{a.displayName}</strong>{/each}
      {#if connected.some((a) => a.privateOnly)}
        <span class="muted">· uploads stay private until your Google project passes YouTube's audit</span>
      {/if}
    </p>
  {/if}
  {#if next}
    <div class="next-action callout {reconnect.length ? 'bad' : ''}">
      <p>{next}</p>
      <button class="primary small" onclick={() => (open = true)}>{reconnect.length ? "Reconnect" : "Set up"}</button>
    </div>
  {/if}
  {#if (!connected.length && !reconnect.length) || open}
    <ol class="checklist">
      {#each STEPS[platform] as step (step.text)}
        <li>
          {step.text}
          {#if step.link}<button class="link" onclick={() => api.openLink(step.link!.url)}>{step.link.label} ↗</button>{/if}
        </li>
      {/each}
    </ol>
  {/if}
  <details bind:open>
    <summary>{connected.length ? "Manage connection" : "Connection details"}</summary>
    {#if platform === "youtube_shorts"}
      <YouTubeSettings {revision} onChanged={changed} />
    {:else if platform === "instagram_reels"}
      <InstagramSettings {revision} onChanged={changed} />
    {:else if platform === "linkedin"}
      <LinkedInSettings {revision} onChanged={changed} />
    {:else}
      <SnapchatSettings {revision} onChanged={changed} />
    {/if}
  </details>
</article>
