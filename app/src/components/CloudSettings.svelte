<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatDateTime } from "../lib/format";
  import type { CloudStatus } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0 }: { revision?: number } = $props();

  let status = $state<CloudStatus | null>(null);
  let busy = $state(false);
  let url = $state("");
  let token = $state("");

  async function load() {
    try {
      status = await api.cloudStatus();
      if (status.url && !url) url = status.url;
    } catch (e) {
      errorToast(e);
    }
  }

  onMount(load);
  $effect(() => {
    void revision;
    load();
  });

  async function act(action: () => Promise<CloudStatus>, message?: string) {
    busy = true;
    try {
      status = await action();
      if (message) toast(message, "good");
    } catch (e) {
      errorToast(e);
    } finally {
      busy = false;
    }
  }

  async function configure(event: SubmitEvent) {
    event.preventDefault();
    try {
      await act(() => api.cloudConfigure(url, token), "Cloud runner connected");
    } finally {
      // The token goes to the OS credential store and is never read back.
      token = "";
    }
  }

  function forget() {
    if (!confirm("Forget the cloud runner's address and token? Cloud mode turns off.")) return;
    url = "";
    return act(() => api.cloudForget(), "Cloud runner forgotten");
  }
</script>

<fieldset class="cloud">
  <legend>Cloud runner (optional)</legend>
  <p class="muted">
    A cloud runner you host posts approved publications while this computer is off. It is off unless you set one up; without
    it everything runs on this computer as before.
  </p>
  {#if status}
    {#if status.nextStep}<p class="muted" data-testid="cloud-next-step">{status.nextStep}</p>{/if}
    {#if status.enabled}
      <p data-testid="cloud-on">
        Cloud mode is on: <code>{status.url}</code>.
        {status.held} publication{status.held === 1 ? "" : "s"} with the runner.
        {#if status.lastSyncAt}Last checked {formatDateTime(status.lastSyncAt)}.{/if}
      </p>
      {#if status.lastError}<p class="note" data-testid="cloud-error">{status.lastError}</p>{/if}
      <div class="row">
        <button disabled={busy} onclick={() => act(() => api.cloudSyncNow())}>Check now</button>
        <button disabled={busy || status.held > 0} onclick={() => act(() => api.cloudSetEnabled(false), "Cloud mode off")}>
          Turn off
        </button>
      </div>
      {#if status.held > 0}<p class="muted">Take its publications back to this computer before turning cloud mode off.</p>{/if}
    {:else if status.url && status.tokenSaved}
      <p>Cloud mode is off. Runner: <code>{status.url}</code></p>
      <div class="row">
        <button disabled={busy} onclick={() => act(() => api.cloudSetEnabled(true), "Cloud mode on")}>Turn on</button>
        <button disabled={busy} onclick={forget}>Forget runner</button>
      </div>
    {/if}
    {#if !status.enabled}
      <form class="form" aria-label="Cloud runner" onsubmit={configure}>
        <label>
          Runner address
          <input bind:value={url} required autocomplete="off" placeholder="https://runner.example.com" />
        </label>
        <label>
          Installation token
          <input type="password" bind:value={token} required autocomplete="off" placeholder="sc_…" />
        </label>
        <div class="row">
          <button class="primary" type="submit" disabled={busy || !url.trim() || !token.trim()}>Connect</button>
        </div>
      </form>
      <p class="muted">
        Create the token on the runner with <code>shorts-runner token create</code>. It is kept in the operating system's
        credential store and is not shown again. Connecting turns cloud mode on; then "Post from the cloud" appears on
        approved publications.
      </p>
    {/if}
  {/if}
</fieldset>
