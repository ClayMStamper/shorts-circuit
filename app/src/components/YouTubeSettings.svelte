<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { YouTubeChannel, YouTubeStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0, onChanged }: { revision?: number; onChanged?: () => void } = $props();

  let status = $state<YouTubeStatus | null>(null);
  let busy = $state(false);
  let editingClient = $state(false);
  let clientId = $state("");
  let clientSecret = $state("");
  let polling: ReturnType<typeof setInterval> | null = null;

  async function load() {
    try {
      status = await api.youtubeStatus();
    } catch (e) {
      errorToast(e);
    }
  }

  onMount(load);
  $effect(() => {
    void revision;
    if (!polling) load();
  });
  onDestroy(stopPolling);

  function stopPolling() {
    if (polling) clearInterval(polling);
    polling = null;
  }

  async function act(fn: () => Promise<YouTubeStatus>, message?: string) {
    busy = true;
    try {
      status = await fn();
      if (message) toast(message, "good");
      onChanged?.();
    } catch (e) {
      errorToast(e);
    } finally {
      busy = false;
    }
  }

  async function saveClient(event: SubmitEvent) {
    event.preventDefault();
    await act(() => api.youtubeSetClient(clientId, clientSecret), "Google OAuth client saved");
    // The secret is never read back; clear it from the form either way.
    clientSecret = "";
    if (status?.clientConfigured) {
      clientId = "";
      editingClient = false;
    }
  }

  async function connect(accountId: string | null) {
    try {
      await api.youtubeConnect(accountId);
    } catch (e) {
      errorToast(e);
      return;
    }
    if (status) status = { ...status, connecting: true };
    stopPolling();
    const started = Date.now();
    polling = setInterval(async () => {
      if (Date.now() - started > 10 * 60_000) {
        stopPolling();
        await load();
        return;
      }
      try {
        const progress = await api.youtubeConnectProgress();
        if (progress.kind === "pending") return;
        stopPolling();
        if (progress.kind === "connected") toast(`${progress.account.displayName} connected`, "good");
        else if (progress.kind === "failed") toast(progress.message, "bad");
        await load();
        onChanged?.();
      } catch (e) {
        stopPolling();
        errorToast(e);
        await load();
      }
    }, 1500);
  }

  async function cancelConnect() {
    stopPolling();
    try {
      await api.youtubeCancelConnect();
    } catch (e) {
      errorToast(e);
    }
    await load();
  }

  function disconnect(c: YouTubeChannel) {
    if (!confirm(`Disconnect ${c.displayName}? Its publications will use export packages until you connect it again.`)) return;
    act(() => api.youtubeDisconnect(c.accountId), "Disconnected");
  }

  function setPublic(c: YouTubeChannel, isPublic: boolean) {
    if (
      isPublic &&
      !confirm(
        "Upload new videos to this channel as public?\n\nOnly do this once your Google Cloud project has passed YouTube's API audit (or YouTube has shown it publishes your uploads publicly). Otherwise YouTube may lock them as private.",
      )
    )
      return;
    act(() => api.youtubeSetPublicUploads(c.accountId, isPublic));
  }

  function pct(used: number, limit: number): number {
    return limit > 0 ? Math.min(100, Math.round((used / limit) * 100)) : 0;
  }
</script>

<fieldset class="youtube">
  <legend>YouTube Shorts publishing</legend>
  {#if !status}
    <p class="muted">Checking…</p>
  {:else}
    <h3>1. Your Google OAuth client</h3>
    {#if status.clientConfigured && !editingClient}
      <p data-testid="client-saved">
        Saved in your system's credential store (Windows Credential Manager).
        <button class="link" onclick={() => (editingClient = true)}>Replace</button>
        <button class="link" disabled={busy} onclick={() => act(() => api.youtubeClearClient(), "Removed")}>Remove</button>
      </p>
    {:else}
      {#if status.nextStep}<p class="muted">{status.nextStep}</p>{/if}
      <form class="form" onsubmit={saveClient} aria-label="Google OAuth client">
        <label>Client ID <input bind:value={clientId} placeholder="…apps.googleusercontent.com" required autocomplete="off" /></label>
        <label>Client secret <input type="password" bind:value={clientSecret} required autocomplete="off" /></label>
        <div class="row">
          <button class="primary" type="submit" disabled={busy}>Save</button>
          {#if status.clientConfigured}<button type="button" onclick={() => (editingClient = false)}>Keep the saved one</button>{/if}
        </div>
      </form>
      <p class="muted">
        Use the "Desktop app" client from your Google Cloud project. Both values are kept in the operating system's
        credential store, never in the app's database, and are not shown again.
      </p>
    {/if}

    <h3>2. Channels</h3>
    {#if status.connecting}
      <p>Finish signing in to Google in your browser…</p>
      <button onclick={cancelConnect}>Stop waiting</button>
    {/if}
    <ul class="list">
      {#each status.channels as c (c.accountId)}
        <li class="channel" data-testid="channel">
          <div>
            <strong>{c.displayName}</strong>
            {#if c.needsReconnect}
              <span class="badge needs_authentication">needs reconnecting</span>
            {:else if c.connected}
              <span class="badge published">connected</span>
            {:else}
              <span class="badge">export packages</span>
            {/if}
          </div>
          {#if c.needsReconnect && c.lastError}<p class="error">{c.lastError}</p>{/if}
          {#if c.connected}
            <p class="muted">
              {#if c.connectedAt}Connected {formatDateTime(c.connectedAt)}.{/if}
              {#if c.signInExpiresAt}Google ends this sign-in around {formatDateTime(c.signInExpiresAt)}; reconnect then.{/if}
            </p>
            <label class="check">
              <input
                type="checkbox"
                checked={!c.privateOnly}
                disabled={busy}
                onchange={(e) => setPublic(c, e.currentTarget.checked)}
              />
              Upload as public (my Google Cloud project has passed YouTube's API audit)
            </label>
            {#if c.privateOnly}
              <p class="note" data-testid="private-note">
                <strong>Uploads stay private for now.</strong> Google keeps videos uploaded by apps whose Google Cloud
                project hasn't passed YouTube's API compliance audit private, so this channel uploads as private until you
                tick the box above. The video still lands on your channel; you'll find it in YouTube Studio.
              </p>
            {/if}
            {#if c.notes.length}
              <ul class="notes">
                {#each c.notes.filter((n) => !n.startsWith("Uploads go up as private")) as n (n)}<li>{n}</li>{/each}
              </ul>
            {/if}
          {/if}
          <div class="row">
            {#if !c.connected || c.needsReconnect}
              <button class="primary" disabled={!status.clientConfigured || status.connecting} onclick={() => connect(c.accountId)}>
                {c.connected ? "Reconnect" : "Connect"} {c.displayName}…
              </button>
            {/if}
            {#if c.connected}<button disabled={busy} onclick={() => disconnect(c)}>Disconnect</button>{/if}
          </div>
        </li>
      {/each}
    </ul>
    <button class="primary" disabled={!status.clientConfigured || status.connecting} onclick={() => connect(null)}>
      Connect a YouTube channel…
    </button>
    <p class="muted">
      Opens Google in your browser. The app asks to upload videos and to see your channel's videos (to follow processing
      and find an upload again if an answer gets lost).
    </p>

    <h3>Today's API quota</h3>
    <div class="quota" data-testid="quota">
      <div>
        Uploads: {status.quota.uploadsUsed} of {status.quota.uploadsLimit}
        <progress max="100" value={pct(status.quota.uploadsUsed, status.quota.uploadsLimit)}></progress>
      </div>
      <div>
        Other calls: {status.quota.unitsUsed} of {status.quota.unitsLimit} units
        <progress max="100" value={pct(status.quota.unitsUsed, status.quota.unitsLimit)}></progress>
      </div>
      <p class="muted">Resets {formatDateTime(status.quota.resetsAt)} (midnight Pacific time).</p>
      {#if status.quota.warning}<p class="error" data-testid="quota-warning">{status.quota.warning}</p>{/if}
    </div>
  {/if}
</fieldset>
