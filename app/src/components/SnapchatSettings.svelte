<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { SnapProfileView, SnapStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0, onChanged }: { revision?: number; onChanged?: () => void } = $props();

  const DEFAULT_REDIRECT = "http://127.0.0.1:47615/snapchat/callback";

  let status = $state<SnapStatus | null>(null);
  let busy = $state(false);
  let editingClient = $state(false);
  let clientId = $state("");
  let clientSecret = $state("");
  let redirectUri = $state(DEFAULT_REDIRECT);
  let pasted = $state("");
  let polling: ReturnType<typeof setInterval> | null = null;

  async function load() {
    try {
      status = await api.snapchatStatus();
    } catch (e) {
      errorToast(e);
    }
  }

  onMount(async () => {
    await load();
    if (status) redirectUri = status.redirectUri;
  });
  $effect(() => {
    void revision;
    if (!polling) load();
  });
  onDestroy(stopPolling);

  function stopPolling() {
    if (polling) clearInterval(polling);
    polling = null;
  }

  async function act(fn: () => Promise<SnapStatus>, message?: string): Promise<boolean> {
    busy = true;
    try {
      status = await fn();
      if (message) toast(message, "good");
      onChanged?.();
      return true;
    } catch (e) {
      errorToast(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function saveClient(event: SubmitEvent) {
    event.preventDefault();
    await act(() => api.snapchatSetClient(clientId, clientSecret, redirectUri), "Snap OAuth app saved");
    // The secret is never read back; clear it from the form either way.
    clientSecret = "";
    if (status?.clientConfigured) {
      clientId = "";
      redirectUri = status.redirectUri;
      editingClient = false;
    }
  }

  function editClient() {
    if (status) redirectUri = status.redirectUri;
    editingClient = true;
  }

  async function connect(accountId: string | null) {
    try {
      await api.snapchatConnect(accountId);
    } catch (e) {
      errorToast(e);
      return;
    }
    if (status?.returnMode !== "loopback") {
      // Paste mode: the browser lands on a page that shows a text to paste back here.
      pasted = "";
      await load();
      return;
    }
    status = { ...status, connecting: true };
    stopPolling();
    const started = Date.now();
    polling = setInterval(async () => {
      if (Date.now() - started > 10 * 60_000) {
        stopPolling();
        await load();
        return;
      }
      try {
        const progress = await api.snapchatConnectProgress();
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

  async function finishWithCode(event: SubmitEvent) {
    event.preventDefault();
    const ok = await act(() => api.snapchatFinishWithCode(pasted), "Snapchat profile connected");
    if (ok) pasted = "";
    else await load();
  }

  async function cancelConnect() {
    stopPolling();
    pasted = "";
    try {
      await api.snapchatCancelConnect();
    } catch (e) {
      errorToast(e);
    }
    await load();
  }

  function disconnect(p: SnapProfileView) {
    if (!confirm(`Disconnect ${p.displayName}? Its publications will use export packages until you connect it again.`)) return;
    act(() => api.snapchatDisconnect(p.accountId), "Disconnected");
  }

  function checkConnection(p: SnapProfileView) {
    act(() => api.snapchatCheckConnection(p.accountId), `${p.displayName} checked`);
  }

  let waiting = $derived(!!status && (status.connecting || status.awaitingCode));
</script>

<fieldset class="snapchat">
  <legend>Snapchat Spotlight publishing</legend>
  {#if !status}
    <p class="muted">Checking…</p>
  {:else}
    <h3>1. Your Snap OAuth app</h3>
    {#if status.clientConfigured && !editingClient}
      <p data-testid="snap-client-saved">
        Saved in your system's credential store (Windows Credential Manager). Redirect address:
        <code>{status.redirectUri}</code>
        <button class="link" onclick={editClient}>Replace</button>
        <button class="link" disabled={busy} onclick={() => act(() => api.snapchatClearClient(), "Removed")}>Remove</button>
      </p>
    {:else}
      {#if status.nextStep}<p class="muted" data-testid="snap-next-step">{status.nextStep}</p>{/if}
      <form class="form" onsubmit={saveClient} aria-label="Snap OAuth app">
        <label>Client ID <input bind:value={clientId} required autocomplete="off" /></label>
        <label>Client secret <input type="password" bind:value={clientSecret} required autocomplete="off" /></label>
        <label>Redirect address <input bind:value={redirectUri} required autocomplete="off" spellcheck="false" /></label>
        <p class="muted" data-testid="snap-redirect-hint">
          Register exactly this address as the redirect URI of your OAuth app in Snapchat Ads Manager. If Snap only accepts
          https, register <code>https://claymstamper.github.io/shorts-circuit/snap-callback.html</code> there and here; the
          page then shows a text to paste back into the app.
        </p>
        <div class="row">
          <button class="primary" type="submit" disabled={busy}>Save</button>
          {#if status.clientConfigured}<button type="button" onclick={() => (editingClient = false)}>Keep the saved one</button>{/if}
        </div>
      </form>
      <p class="muted">
        Create the app in Snapchat Ads Manager (Business Details → OAuth apps). The client ID and secret are kept in the
        operating system's credential store, never in the app's database, and are not shown again.
      </p>
    {/if}

    <h3>2. Public profiles</h3>
    {#if status.connecting}
      <p>Finish signing in to Snapchat in your browser…</p>
      <button onclick={cancelConnect}>Stop waiting</button>
    {/if}
    {#if status.awaitingCode}
      <form class="form" onsubmit={finishWithCode} aria-label="Snap sign-in code">
        <p>Sign in to Snapchat in your browser. The page it ends on shows a text; copy it and paste it here.</p>
        <label>
          Paste the text from the page
          <input bind:value={pasted} required autocomplete="off" spellcheck="false" />
        </label>
        <div class="row">
          <button class="primary" type="submit" disabled={busy || !pasted.trim()}>Finish connecting</button>
          <button type="button" onclick={cancelConnect}>Cancel</button>
        </div>
      </form>
    {/if}
    <ul class="list">
      {#each status.profiles as p (p.accountId)}
        <li class="channel" data-testid="snap-profile">
          <div>
            <strong>{p.displayName}</strong>
            {#if p.needsReconnect}
              <span class="badge needs_authentication">needs reconnecting</span>
            {:else if p.connected}
              <span class="badge published">connected</span>
            {:else}
              <span class="badge">export packages</span>
            {/if}
          </div>
          {#if p.lastError}<p class="error">{p.lastError}</p>{/if}
          {#if p.connected}
            {#if p.connectedAt}<p class="muted">Connected {formatDateTime(p.connectedAt)}.</p>{/if}
            {#if p.notes.length}
              <ul class="notes">
                {#each p.notes as n (n)}<li>{n}</li>{/each}
              </ul>
            {/if}
          {/if}
          <div class="row">
            {#if !p.connected || p.needsReconnect}
              <button class="primary" disabled={!status.clientConfigured || waiting} onclick={() => connect(p.accountId)}>
                {p.connected ? "Reconnect" : "Connect"} {p.displayName}…
              </button>
            {/if}
            {#if p.connected}
              <button disabled={busy} onclick={() => checkConnection(p)}>Check connection</button>
              <button disabled={busy} onclick={() => disconnect(p)}>Disconnect</button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
    <button class="primary" disabled={!status.clientConfigured || waiting} onclick={() => connect(null)}>
      Connect a Snapchat public profile…
    </button>
    <p class="muted">
      Opens Snapchat in your browser. Posting to Spotlight needs your Snap OAuth app allowlisted for the Public Profile
      API; until Snap does that, sign-in or posting is refused and publications use export packages.
    </p>
  {/if}
</fieldset>
