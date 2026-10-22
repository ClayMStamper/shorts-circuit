<script lang="ts">
  import { untrack } from "svelte";
  import { api } from "../lib/api";
  import type { WorkflowSuggestion } from "../lib/types";
  import Drawer from "./ui/Drawer.svelte";
  import PlatformIcon from "./ui/PlatformIcon.svelte";
  import Switch from "./ui/Switch.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    suggestions,
    onclose,
    onCreated,
    onConnections,
  }: {
    suggestions: WorkflowSuggestion[];
    onclose: () => void;
    onCreated: () => void;
    onConnections: () => void;
  } = $props();

  let accountId = $state(untrack(() => suggestions[0]?.accountId ?? ""));
  let name = $state("");
  let autoPublish = $state(true);
  let rights = $state(false);
  let busy = $state(false);

  const chosen = $derived(suggestions.find((s) => s.accountId === accountId));

  async function create(e: SubmitEvent) {
    e.preventDefault();
    if (!chosen) return;
    busy = true;
    try {
      await api.createWorkflow({
        accountId,
        name: name.trim() || null,
        autoPublish,
        rightsConfirmed: rights,
      });
      toast(`${name.trim() || chosen.name} created`, "good");
      onCreated();
    } catch (err) {
      errorToast(err);
    } finally {
      busy = false;
    }
  }
</script>

<Drawer title="Create workflow" {onclose}>
  {#if suggestions.length === 0}
    <p>Every connected destination already has a workflow.</p>
    <p class="muted">Connect another YouTube channel, Instagram account, LinkedIn page or Snapchat profile first.</p>
    <div class="btn-row"><button class="primary" onclick={onConnections}>Go to Connections</button></div>
  {:else}
    <form class="form" id="create-workflow" onsubmit={create}>
      <fieldset class="setting" style="border: none; padding: 0">
        <legend class="label">Destination</legend>
        {#each suggestions as s (s.accountId)}
          <label class="check">
            <input type="radio" name="destination" value={s.accountId} bind:group={accountId} />
            <PlatformIcon platform={s.platform} size={20} />
            {s.platformName} · {s.accountName}
          </label>
        {/each}
      </fieldset>
      <div class="setting">
        <label class="label" for="new-wf-name">Name</label>
        <input id="new-wf-name" bind:value={name} placeholder={chosen?.name ?? ""} />
      </div>
      <div class="setting">
        <Switch checked={autoPublish} label="Auto Publish" onchange={(v) => (autoPublish = v)} />
        <p class="help">
          {autoPublish
            ? "New videos are published automatically, with the caption and hashtags from the source."
            : "New videos are prepared and wait for you to approve them."}
          Only videos that arrive from now on are included.
        </p>
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={rights} />
        I own or have the rights to content from this source
      </label>
      {#if !rights && autoPublish}
        <p class="help warn">Without this, each video waits for you to record its video and audio permissions.</p>
      {/if}
    </form>
  {/if}
  {#snippet footer()}
    <button type="button" onclick={onclose}>Cancel</button>
    {#if suggestions.length}
      <button class="primary" type="submit" form="create-workflow" disabled={busy || !chosen}>Create workflow</button>
    {/if}
  {/snippet}
</Drawer>
