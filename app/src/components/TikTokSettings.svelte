<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { TikTokStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0 }: { revision?: number } = $props();

  let status = $state<TikTokStatus | null>(null);
  let busy = $state(false);
  let polling: ReturnType<typeof setInterval> | null = null;

  async function load() {
    try {
      status = await api.tiktokStatus();
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
    TikTok doesn't let apps download your original videos. When this is on, the app only checks your public post list
    so each new post shows up in the library as an item awaiting its file; when the original lands in the watch folder
    (synced from your phone or editor), you confirm the match.
  </p>
  {#if !status}
    <p class="muted">Checking…</p>
  {:else if status.kind === "unavailable"}
    <p data-testid="tiktok-unavailable"><strong>Unavailable.</strong> {status.reason}</p>
    <p><strong>Next step:</strong> {status.nextStep}</p>
  {:else if status.kind === "off"}
    <label class="check">
      <input type="checkbox" checked={false} disabled={busy} onchange={() => act(() => api.tiktokSetEnabled(true))} />
      Detect new TikTok posts
    </label>
  {:else}
    <label class="check">
      <input type="checkbox" checked={true} disabled={busy} onchange={() => act(() => api.tiktokSetEnabled(false))} />
      Detect new TikTok posts
    </label>
    {#if status.kind === "notConnected"}
      <button class="primary" onclick={connect}>Connect TikTok…</button>
      <p class="muted">Opens TikTok in your browser. The app asks only for your profile name and public post list.</p>
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
