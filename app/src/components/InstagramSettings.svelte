<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { InstagramAccount, InstagramLogin, InstagramPageChoice, InstagramStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0, onChanged }: { revision?: number; onChanged?: () => void } = $props();

  let status = $state<InstagramStatus | null>(null);
  let busy = $state(false);
  let login = $state<InstagramLogin>("facebook_login");
  let token = $state("");
  /** The Pages the pasted token manages, once looked up (Facebook login). */
  let pages = $state<InstagramPageChoice[] | null>(null);
  let pageId = $state("");
  let warning = $state<string | null>(null);
  /** The account a pasted token reconnects, or null for a new one. */
  let reconnecting = $state<InstagramAccount | null>(null);

  const linked = $derived((pages ?? []).filter((p) => p.instagramUsername));

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

  function reset() {
    // The token is never read back; clear it from the form either way.
    token = "";
    pages = null;
    pageId = "";
  }

  function startReconnect(a: InstagramAccount) {
    reconnecting = a;
    login = "facebook_login";
    reset();
  }

  async function connectTo(page: string | null) {
    status = await api.instagramConnect(token, login, page, reconnecting?.accountId ?? null);
    toast(
      reconnecting
        ? `${reconnecting.displayName} reconnected`
        : login === "facebook_login"
          ? "Instagram account connected"
          : "Instagram account added (export packages only)",
      "good",
    );
    reconnecting = null;
    reset();
    onChanged?.();
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    try {
      if (login === "instagram_login") {
        await connectTo(null);
      } else if (pages === null) {
        // First look the Pages up; one linked Page is connected straight away.
        const found = await api.instagramFindPages(token);
        warning = found.warning;
        const usable = found.pages.filter((p) => p.instagramUsername);
        if (usable.length === 1 && usable[0]) {
          await connectTo(usable[0].pageId);
        } else {
          pages = found.pages;
          pageId = "";
        }
      } else {
        await connectTo(pageId);
      }
    } catch (e) {
      reset();
      errorToast(e);
    } finally {
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
            {#if a.pageName}<span class="muted"> · Page: {a.pageName}</span>{/if}
            {#if a.connected && a.login === "instagram_login"}
              <span class="badge needs_authentication">reconnect with Facebook login</span>
            {:else if a.needsReconnect}
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
              {#if a.canPublish}Publishes as @{a.username} through the Facebook Page {a.pageName}.{/if}
              {#if a.connectedAt}Connected {formatDateTime(a.connectedAt)}.{/if}
            </p>
            {#if a.notes.length && a.canPublish}
              <ul class="notes">
                {#each a.notes as n (n)}<li>{n}</li>{/each}
              </ul>
            {/if}
          {/if}
          <div class="row">
            {#if !a.connected || a.needsReconnect}
              <button class="primary" disabled={busy} onclick={() => startReconnect(a)}>
                {a.login === "instagram_login"
                  ? `Reconnect ${a.displayName} with Facebook login…`
                  : `${a.connected ? "Reconnect" : "Connect"} ${a.displayName}…`}
              </button>
            {/if}
            {#if a.connected}<button disabled={busy} onclick={() => disconnect(a)}>Disconnect</button>{/if}
          </div>
        </li>
      {/each}
    </ul>

    <form class="form" onsubmit={submit} aria-label="Instagram access token">
      <h3>{reconnecting ? `Reconnect ${reconnecting.displayName}` : "Connect an Instagram account"}</h3>
      <div class="row" role="radiogroup" aria-label="Connection type">
        <label>
          <input type="radio" name="ig-login" value="facebook_login" bind:group={login} onchange={reset} />
          Facebook login (recommended)
        </label>
        <label>
          <input type="radio" name="ig-login" value="instagram_login" bind:group={login} onchange={reset} />
          Instagram login (export packages only)
        </label>
      </div>
      {#if login === "instagram_login"}
        <p class="callout" data-testid="instagram-login-limit">
          Instagram login can't upload files directly: Instagram only takes a public video address that way, and
          Shorts Circuit doesn't host your videos. Accounts connected like this get export packages to post by hand. Use
          Facebook login to publish automatically.
        </p>
      {/if}
      <label>
        {login === "facebook_login" ? "Long-lived user access token" : "Instagram login token"}
        <input
          type="password"
          bind:value={token}
          required
          autocomplete="off"
          placeholder={login === "facebook_login" ? "EAA…" : "IGAA…"}
          oninput={() => (pages = null)}
        />
      </label>
      {#if pages !== null}
        <fieldset class="pages" data-testid="instagram-pages">
          <legend>Choose the Page your Instagram account is linked to</legend>
          {#each pages as p (p.pageId)}
            <label>
              <input type="radio" name="ig-page" value={p.pageId} bind:group={pageId} disabled={!p.instagramUsername} />
              <strong>{p.pageName}</strong>
              {#if p.instagramUsername}
                · @{p.instagramUsername}
              {:else}
                <span class="muted">· no linked Instagram account (link it in the Page's settings → Linked accounts)</span>
              {/if}
            </label>
          {/each}
        </fieldset>
      {/if}
      {#if warning}<p class="callout">{warning}</p>{/if}
      <div class="row">
        <button
          class="primary"
          type="submit"
          disabled={busy || !token.trim() || (pages !== null && (!pageId || !linked.length))}
        >
          {#if login === "facebook_login" && pages === null}
            Find my Pages
          {:else}
            {reconnecting ? "Reconnect" : "Connect"}
          {/if}
        </button>
        {#if reconnecting}<button type="button" onclick={() => ((reconnecting = null), reset())}>Cancel</button>{/if}
      </div>
    </form>
    {#if login === "facebook_login"}
      <p class="muted">
        In Graph API Explorer, pick your Meta app, add the permissions instagram_basic, instagram_content_publish,
        pages_show_list, pages_read_engagement and business_management, and click Generate Access Token. Open it in the
        Access Token Debugger and click Extend Access Token, then paste the long-lived token here. The app keeps only the
        Page's token (it doesn't expire) in the operating system's credential store, never in its database, and doesn't
        show it again.
      </p>
      <p class="muted">The same Page connection could later publish Facebook Reels too (not available yet).</p>
    {/if}
  {/if}
</fieldset>
