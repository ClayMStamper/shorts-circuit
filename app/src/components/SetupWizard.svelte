<script lang="ts">
  import { onMount, tick } from "svelte";
  import { api } from "../lib/api";
  import { PLATFORMS, type WorkflowSuggestion } from "../lib/types";
  import DestinationCard from "./DestinationCard.svelte";
  import SourceCard from "./SourceCard.svelte";
  import PlatformIcon from "./ui/PlatformIcon.svelte";
  import Switch from "./ui/Switch.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let { onDone, onCancel }: { onDone: () => void; onCancel?: () => void } = $props();

  const STEPS = ["Source", "Destinations", "Workflows"];
  let step = $state(0);
  let revision = $state(0);
  let suggestions = $state<WorkflowSuggestion[]>([]);
  let picked = $state<string[]>([]);
  let autoPublish = $state(true);
  let rights = $state(false);
  let busy = $state(false);
  let heading: HTMLHeadingElement | undefined = $state();

  async function loadSuggestions() {
    try {
      suggestions = (await api.listWorkflows()).suggestions;
      picked = suggestions.map((s) => s.accountId);
    } catch (e) {
      errorToast(e);
    }
  }

  onMount(loadSuggestions);

  async function go(next: number) {
    step = next;
    if (next === 2) await loadSuggestions();
    await tick();
    heading?.focus();
  }

  async function create() {
    busy = true;
    let made = 0;
    try {
      for (const accountId of picked) {
        await api.createWorkflow({ accountId, autoPublish, rightsConfirmed: rights });
        made += 1;
      }
      toast(`${made} workflow${made === 1 ? "" : "s"} created`, "good");
      onDone();
    } catch (e) {
      errorToast(e);
      if (made) onDone();
    } finally {
      busy = false;
    }
  }
</script>

<section class="wizard" aria-labelledby="wizard-title">
  <div class="page-head">
    <div>
      <h1 id="wizard-title">Set up Shorts Circuit</h1>
      <p class="subtitle">Connect your accounts once, choose where your videos arrive, and let them flow.</p>
    </div>
    {#if onCancel}<button onclick={onCancel}>Close setup</button>{/if}
  </div>

  <ol class="steps" aria-label="Setup steps">
    {#each STEPS as name, i (name)}
      <li aria-current={i === step ? "step" : undefined} class:done={i < step}>{name}</li>
    {/each}
  </ol>

  {#if step === 0}
    <h2 tabindex="-1" bind:this={heading}>1. Where do your videos come from?</h2>
    <SourceCard {revision} />
    <div class="btn-row" style="justify-content: flex-end">
      <button class="primary" onclick={() => go(1)}>Next: destinations</button>
    </div>
  {:else if step === 1}
    <h2 tabindex="-1" bind:this={heading}>2. Where should they go?</h2>
    <p class="muted">Connect at least one. Each card lists what to do in that platform's console, once.</p>
    <div class="cards">
      {#each PLATFORMS as p (p)}
        <DestinationCard platform={p} {revision} onChanged={() => (revision += 1)} />
      {/each}
    </div>
    <div class="btn-row" style="justify-content: space-between">
      <button onclick={() => go(0)}>Back</button>
      <button class="primary" onclick={() => go(2)}>Next: workflows</button>
    </div>
  {:else}
    <h2 tabindex="-1" bind:this={heading}>3. Your workflows</h2>
    {#if suggestions.length === 0}
      <p class="callout warn">No destination is connected yet. Go back and connect one; this step then suggests a workflow for it.</p>
      <div class="btn-row"><button onclick={() => go(1)}>Back to destinations</button></div>
    {:else}
      <p class="muted">One workflow per destination. Content that arrives from now on is published automatically, with the caption and hashtags from the source.</p>
      <fieldset style="border: none; padding: 0">
        <legend class="sr-only">Workflows to create</legend>
        {#each suggestions as s (s.accountId)}
          <label class="suggestion">
            <span class="flow" aria-hidden="true"><PlatformIcon platform="tiktok" />→<PlatformIcon platform={s.platform} /></span>
            <span><strong>{s.name}</strong> <span class="muted">· {s.accountName}</span></span>
            <input type="checkbox" value={s.accountId} bind:group={picked} aria-label="Create {s.name}" />
          </label>
        {/each}
      </fieldset>
      <div class="setting">
        <Switch checked={autoPublish} label="Auto Publish" onchange={(v) => (autoPublish = v)} />
        <p class="help">
          {autoPublish ? "Publish new videos automatically, starting now." : "Prepare new videos and wait for my review."}
          You can change this per workflow later.
        </p>
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={rights} />
        I own or have the rights to content from this source
      </label>
      <p class="help muted" style="margin: 0">
        Records video and audio permission for these videos. TikTok sounds are often licensed for TikTok only; leave this
        off if you use them, and each video waits for you instead.
      </p>
      <div class="btn-row" style="justify-content: space-between">
        <button onclick={() => go(1)}>Back</button>
        <button class="primary" disabled={busy || picked.length === 0} onclick={create}>
          Create {picked.length} workflow{picked.length === 1 ? "" : "s"}
        </button>
      </div>
    {/if}
  {/if}
</section>
