<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { LinkedInOrganization, LinkedInPage, LinkedInStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0, onChanged }: { revision?: number; onChanged?: () => void } = $props();

  let status = $state<LinkedInStatus | null>(null);
  let busy = $state(false);
  let token = $state("");
  /** The page id, URN or admin address to post as. */
  let page = $state("");
  let found = $state<LinkedInOrganization[] | null>(null);
  /** The page a pasted token reconnects, or null for a new one. */
  let reconnecting = $state<LinkedInPage | null>(null);

  async function load() {
    try {
      status = await api.linkedinStatus();
    } catch (e) {
      errorToast(e);
    }
  }

  onMount(load);
  $effect(() => {
    void revision;
    load();
  });

  async function findPages() {
    busy = true;
    try {
      found = await api.linkedinFindPages(token);
      if (found.length === 1 && found[0]) page = found[0].urn;
      if (found.length === 0) toast("This token's member administers no LinkedIn pages.", "bad");
    } catch (e) {
      found = null;
      errorToast(e);
    } finally {
      busy = false;
    }
  }

  async function connect(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    try {
      status = await api.linkedinConnect(token, page, reconnecting?.accountId ?? null);
      toast(reconnecting ? `${reconnecting.displayName} reconnected` : "LinkedIn page connected", "good");
      reconnecting = null;
      page = "";
      found = null;
      onChanged?.();
    } catch (e) {
      errorToast(e);
    } finally {
      // The token is never read back; clear it from the form either way.
      token = "";
      busy = false;
    }
  }

  async function disconnect(p: LinkedInPage) {
    if (!confirm(`Disconnect ${p.displayName}? Its publications will use export packages until you connect it again.`)) return;
    busy = true;
    try {
      status = await api.linkedinDisconnect(p.accountId);
      toast("Disconnected", "good");
      onChanged?.();
    } catch (e) {
      errorToast(e);
    } finally {
      busy = false;
    }
  }
</script>

<fieldset class="linkedin">
  <legend>LinkedIn page posts</legend>
  {#if !status}
    <p class="muted">Checking…</p>
  {:else}
    {#if status.nextStep}<p class="muted" data-testid="linkedin-next-step">{status.nextStep}</p>{/if}
    <ul class="list">
      {#each status.pages as p (p.accountId)}
        <li class="channel" data-testid="linkedin-page">
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
          {#if p.needsReconnect && p.lastError}<p class="error">{p.lastError}</p>{/if}
          {#if p.expiresSoon && p.tokenExpiresAt}
            <p class="error" data-testid="expires-soon">
              The LinkedIn token runs out {formatDateTime(p.tokenExpiresAt)}. Generate a new one and reconnect before then.
            </p>
          {/if}
          {#if p.connected}
            <p class="muted">
              {#if p.connectedAt}Connected {formatDateTime(p.connectedAt)}.{/if}
              {#if p.tokenExpiresAt && !p.expiresSoon}Token good until {formatDateTime(p.tokenExpiresAt)}.{/if}
            </p>
            {#if p.notes.length}
              <ul class="notes">
                {#each p.notes as n (n)}<li>{n}</li>{/each}
              </ul>
            {/if}
          {/if}
          <div class="row">
            {#if !p.connected || p.needsReconnect || p.expiresSoon}
              <button class="primary" disabled={busy} onclick={() => (reconnecting = p)}>
                {p.connected ? "Reconnect" : "Connect"} {p.displayName}…
              </button>
            {/if}
            {#if p.connected}<button disabled={busy} onclick={() => disconnect(p)}>Disconnect</button>{/if}
          </div>
        </li>
      {/each}
    </ul>

    <form class="form" onsubmit={connect} aria-label="LinkedIn access token">
      <h3>{reconnecting ? `Reconnect ${reconnecting.displayName}` : "Connect a LinkedIn page"}</h3>
      <label>
        Access token
        <input type="password" bind:value={token} required autocomplete="off" placeholder="AQV…" />
      </label>
      <div class="row">
        <button type="button" disabled={busy || !token.trim()} onclick={findPages}>Find my pages</button>
      </div>
      {#if found && found.length > 1}
        <label>
          Page
          <select bind:value={page}>
            <option value="">Choose a page…</option>
            {#each found as o (o.urn)}<option value={o.urn}>{o.name}</option>{/each}
          </select>
        </label>
      {:else}
        <label>
          Page id
          <input bind:value={page} required autocomplete="off" placeholder="the number in linkedin.com/company/<id>/admin" />
        </label>
        {#if found && found.length === 1 && found[0]}<p class="muted">Found: {found[0].name}</p>{/if}
      {/if}
      <div class="row">
        <button class="primary" type="submit" disabled={busy || !token.trim() || !page.trim()}>
          {reconnecting ? "Reconnect" : "Connect"}
        </button>
        {#if reconnecting}<button type="button" onclick={() => (reconnecting = null)}>Cancel</button>{/if}
      </div>
    </form>
    <p class="muted">
      Generate the token with your LinkedIn app's token generator (developer portal → your app → Auth → OAuth 2.0 tools),
      choosing w_organization_social, r_organization_social and rw_organization_admin. It is kept in the operating
      system's credential store, never in the app's database, and is not shown again. LinkedIn tokens last 60 days.
    </p>
  {/if}
</fieldset>
