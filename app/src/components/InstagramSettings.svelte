<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { InstagramAccount, InstagramStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0, onChanged }: { revision?: number; onChanged?: () => void } = $props();

  let status = $state<InstagramStatus | null>(null);
  let busy = $state(false);
  let token = $state("");
  /** The account a pasted token reconnects, or null for a new one. */
  let reconnecting = $state<InstagramAccount | null>(null);

  async function load() {
    try {
      status = await api.instagramStatus();
    } catch (e) {
      errorToast(e);
    }
  }

  onMount(load);
  $effect(() => {
    void revision;
    load();
  });

  async function connect(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    try {
      status = await api.instagramConnect(token, reconnecting?.accountId ?? null);
      toast(reconnecting ? `${reconnecting.displayName} reconnected` : "Instagram account connected", "good");
      reconnecting = null;
      onChanged?.();
    } catch (e) {
      errorToast(e);
    } finally {
      // The token is never read back; clear it from the form either way.
      token = "";
      busy = false;
    }
  }

  async function disconnect(a: InstagramAccount) {
    if (!confirm(`Disconnect ${a.displayName}? Its publications will use export packages until you connect it again.`)) return;
    busy = true;
    try {
      status = await api.instagramDisconnect(a.accountId);
      toast("Disconnected", "good");
      onChanged?.();
    } catch (e) {
      errorToast(e);
    } finally {
      busy = false;
    }
  }
</script>

<fieldset class="instagram">
  <legend>Instagram Reels publishing</legend>
  {#if !status}
    <p class="muted">Checking…</p>
  {:else}
    {#if status.nextStep}<p class="muted" data-testid="instagram-next-step">{status.nextStep}</p>{/if}
    <ul class="list">
      {#each status.accounts as a (a.accountId)}
        <li class="channel" data-testid="instagram-account">
          <div>
            <strong>{a.displayName}</strong>
            {#if a.needsReconnect}
              <span class="badge needs_authentication">needs reconnecting</span>
            {:else if a.connected}
              <span class="badge published">connected</span>
            {:else}
              <span class="badge">export packages</span>
            {/if}
          </div>
          {#if a.needsReconnect && a.lastError}<p class="error">{a.lastError}</p>{/if}
          {#if a.connected}
            <p class="muted">
              {#if a.connectedAt}Connected {formatDateTime(a.connectedAt)}.{/if}
              {#if a.renewedAt}Sign-in renewed {formatDateTime(a.renewedAt)}.{/if}
              {#if a.tokenExpiresAt}Good until {formatDateTime(a.tokenExpiresAt)}; the app renews it automatically while it runs.{/if}
            </p>
            {#if a.notes.length}
              <ul class="notes">
                {#each a.notes as n (n)}<li>{n}</li>{/each}
              </ul>
            {/if}
          {/if}
          <div class="row">
            {#if !a.connected || a.needsReconnect}
              <button class="primary" disabled={busy} onclick={() => (reconnecting = a)}>
                {a.connected ? "Reconnect" : "Connect"} {a.displayName}…
              </button>
            {/if}
            {#if a.connected}<button disabled={busy} onclick={() => disconnect(a)}>Disconnect</button>{/if}
          </div>
        </li>
      {/each}
    </ul>

    <form class="form" onsubmit={connect} aria-label="Instagram access token">
      <h3>{reconnecting ? `Reconnect ${reconnecting.displayName}` : "Connect an Instagram account"}</h3>
      <label>
        Access token
        <input type="password" bind:value={token} required autocomplete="off" placeholder="IGAA…" />
      </label>
      <div class="row">
        <button class="primary" type="submit" disabled={busy || !token.trim()}>
          {reconnecting ? "Reconnect" : "Connect"}
        </button>
        {#if reconnecting}<button type="button" onclick={() => (reconnecting = null)}>Cancel</button>{/if}
      </div>
    </form>
    <p class="muted">
      In your Meta app's dashboard, open Instagram → API setup with Instagram login, add your professional account, and
      click Generate token. The token is kept in the operating system's credential store, never in the app's database,
      and is not shown again.
    </p>
  {/if}
</fieldset>
