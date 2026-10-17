<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { parseHashtags } from "../lib/format";
  import { CAPTION_LIMITS, captionLength } from "../lib/limits";
  import type { AccountView, ScheduleInput, VariantView } from "../lib/types";
  import ScheduleControl from "./ScheduleControl.svelte";
  import { errorToast } from "./Toasts.svelte";

  let {
    assetId,
    variants,
    onCreated,
    onCancel,
  }: {
    assetId: string;
    variants: VariantView[];
    onCreated: (publicationId: string) => void;
    onCancel: () => void;
  } = $props();

  let accounts = $state<AccountView[]>([]);
  let accountId = $state<string | null>(null);
  let variantId = $state<string>("");
  let caption = $state("");
  let tagsText = $state("");
  let schedule = $state<ScheduleInput | null>(null);

  onMount(async () => {
    variantId = variants[0]?.id ?? "";
    try {
      accounts = await api.listAccounts();
      accountId = accounts[0]?.id ?? null;
    } catch (e) {
      errorToast(e);
    }
  });

  let account = $derived(accounts.find((a) => a.id === accountId) ?? null);
  let hashtags = $derived(parseHashtags(tagsText));
  let length = $derived(captionLength(caption, hashtags));
  let limit = $derived(account ? CAPTION_LIMITS[account.platform] : null);

  async function create(event: SubmitEvent) {
    event.preventDefault();
    if (!accountId) return;
    try {
      const p = await api.createPublication({
        assetId,
        variantId,
        accountId,
        caption,
        hashtags,
        schedule,
      });
      onCreated(p.id);
    } catch (e) {
      errorToast(e);
    }
  }
</script>

<form class="form panel" onsubmit={create}>
  <h3>New publication</h3>
  {#if accounts.length === 0}
    <p class="muted">Add a destination account in Settings first.</p>
  {/if}
  <label>
    Destination
    <select bind:value={accountId} required>
      {#each accounts as a (a.id)}
        <option value={a.id}>{a.platformName} · {a.displayName}{a.automatic ? "" : " (export package)"}</option>
      {/each}
    </select>
  </label>
  <label>
    Variant
    <select bind:value={variantId}>
      {#each variants as v (v.id)}<option value={v.id}>{v.name}</option>{/each}
    </select>
  </label>
  <label>
    Caption
    <textarea rows="4" bind:value={caption}></textarea>
  </label>
  <label>
    Hashtags
    <input placeholder="#recipe #baking" bind:value={tagsText} />
  </label>
  {#if limit}
    <p class="muted" class:error={length > limit.chars || hashtags.length > limit.hashtags}>
      {length}/{limit.chars} characters · {hashtags.length}/{limit.hashtags} hashtags
    </p>
  {/if}
  <ScheduleControl bind:value={schedule} {accountId} />
  <div class="actions">
    <button class="primary" type="submit" disabled={!accountId || !variantId}>Create draft</button>
    <button type="button" onclick={onCancel}>Cancel</button>
  </div>
</form>
