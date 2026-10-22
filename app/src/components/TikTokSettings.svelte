<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { TikTokAppInfo, TikTokStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0 }: { revision?: number } = $props();

  let status = $state<TikTokStatus | null>(null);
  let app = $state<TikTokAppInfo | null>(null);
  let editingApp = $state(false);
  let clientKey = $state("");
  let clientSecret = $state("");
  let busy = $state(false);
  let polling: ReturnType<typeof setInterval> | null = null;

  async function load() {
    try {
      status = await api.tiktokStatus();
    } catch (e) {
      errorToast(e);
    }
    // Older shells (and tests) may not know about the owner's own app; that's fine.
    api
      .tiktokAppInfo()
      .then((info) => (app = info))
      .catch(() => {});
  }

  async function saveApp(event: SubmitEvent) {
    event.preventDefault();
    await act(() => api.tiktokSetApp(clientKey, clientSecret));
    // The secret is never read back; clear it from the form either way.
    clientSecret = "";
    clientKey = "";
    editingApp = false;
    app = await api.tiktokAppInfo().catch(() => app);
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

  async function act(fn: () => Promise<TikTokStatus>) {
    busy = true;
    try {
      status = await fn();
    } catch (e) {
      errorToast(e);
    } finally {
      busy = false;
    }
  }

  async function connect() {
    try {
      await api.tiktokConnect();
      status = { kind: "connecting" };
    } catch (e) {
      errorToast(e);
      return;
    }
    stopPolling();
    const started = Date.now();
    polling = setInterval(async () => {
      if (Date.now() - started > 10 * 60_000) {
        stopPolling();
        await load();
        return;
      }
      try {
        const progress = await api.tiktokFinishConnect();
        if (progress.kind === "pending") return;
        stopPolling();
        if (progress.kind === "connected") {
          status = progress.status;
          toast("TikTok connected", "good");
        } else {
          toast(progress.message, "bad");
          await load();
        }
      } catch (e) {
        stopPolling();
        errorToast(e);
        await load();
      }
    }, 2000);
  }

  async function checkNow() {
    busy = true;
    try {
      const report = await api.tiktokCheckNow();
      toast(
        report.newPosts === 0
          ? "No new TikTok posts"
          : `${report.newPosts} new TikTok post(s) added; save their originals to the watch folder`,
        "good",
      );
      await load();
    } catch (e) {
      errorToast(e);
      await load();
    } finally {
      busy = false;
    }
  }
</script>

<fieldset class="tiktok">
  <legend>TikTok new-post detection (optional)</legend>
  <p class="muted">
    TikTok doesn't let apps download your original videos. When this is on, the app only reads your public post list,
    so when the original lands in the watch folder (synced from your phone or editor) it is matched to its post and
    workflows use that post's caption and hashtags.
  </p>
  {#snippet appForm()}
    <form class="form" onsubmit={saveApp} aria-label="TikTok app">
      <label>Client key <input bind:value={clientKey} required autocomplete="off" /></label>
      <label>Client secret <input type="password" bind:value={clientSecret} required autocomplete="off" /></label>
      <div class="row">
        <button class="primary" type="submit" disabled={busy}>Save</button>
        {#if app?.appConfigured}<button type="button" onclick={() => (editingApp = false)}>Keep the saved one</button>{/if}
      </div>
      <p class="muted">
        From your app at developers.tiktok.com (Login Kit for desktop, scopes user.info.basic and video.list, redirect URI
        <code>{app?.redirectUri ?? "http://127.0.0.1:47616/callback/"}</code>). Both are kept in your system's credential
        store and only sent to TikTok.
      </p>
    </form>
  {/snippet}
  {#if !status}
    <p class="muted">Checking…</p>
  {:else if status.kind === "unavailable"}
    <p data-testid="tiktok-unavailable"><strong>Unavailable.</strong> {status.reason}</p>
    <p><strong>Next step:</strong> {status.nextStep}</p>
    {#if app}{@render appForm()}{/if}
  {:else if status.kind === "off"}
    <label class="check">
      <input type="checkbox" checked={false} disabled={busy} onchange={() => act(() => api.tiktokSetEnabled(true))} />
      Detect new TikTok posts
    </label>
    {#if app?.appConfigured && !editingApp}
      <p class="muted">Your TikTok app ({app.clientKey}) is saved. <button class="link" onclick={() => (editingApp = true)}>Replace</button></p>
    {:else if editingApp}{@render appForm()}{/if}
  {:else}
    <label class="check">
      <input type="checkbox" checked={true} disabled={busy} onchange={() => act(() => api.tiktokSetEnabled(false))} />
      Detect new TikTok posts
    </label>
    {#if status.kind === "notConnected"}
      <button class="primary" onclick={connect}>Connect TikTok…</button>
      <p class="muted">Opens TikTok in your browser. The app asks only for your profile name and public post list.</p>
      {#if app?.appConfigured && !editingApp}
        <p class="muted">Using your TikTok app ({app.clientKey}). <button class="link" onclick={() => (editingApp = true)}>Replace</button></p>
      {:else if editingApp}{@render appForm()}{/if}
    {:else if status.kind === "connecting"}
      <p>Finish signing in to TikTok in your browser…</p>
      <button onclick={connect}>Open the sign-in page again</button>
    {:else if status.kind === "connected"}
      <p>
        Connected as <strong>{status.displayName}</strong>.
        {#if status.lastCheckedAt}Last checked {formatDateTime(status.lastCheckedAt)}.{/if}
        {status.pendingPosts} post(s) waiting for their original file.
      </p>
      {#if status.lastError}<p class="error">Last check failed: {status.lastError}</p>{/if}
      <div class="row">
        <button onclick={checkNow} disabled={busy}>Check now</button>
        <button onclick={() => act(() => api.tiktokDisconnect())} disabled={busy}>Disconnect</button>
      </div>
    {:else if status.kind === "reconnect"}
      <p class="error">{status.message}</p>
      <div class="row">
        <button class="primary" onclick={connect}>Reconnect {status.displayName}…</button>
        <button onclick={() => act(() => api.tiktokDisconnect())} disabled={busy}>Disconnect</button>
      </div>
    {/if}
  {/if}
</fieldset>
