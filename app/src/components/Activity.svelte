<script lang="ts">
  import { api } from "../lib/api";
  import { PLATFORMS, type ActivityView, type Platform } from "../lib/types";
  import { platformLabel } from "../lib/format";
  import ContentItem from "./ContentItem.svelte";
  import EmptyState from "./ui/EmptyState.svelte";
  import { errorToast } from "./Toasts.svelte";

  let {
    revision = 0,
    onConnections,
    onReview,
  }: { revision?: number; onConnections: () => void; onReview?: (id: string) => void } = $props();

  let view = $state<ActivityView | null>(null);
  let platform = $state<Platform | "all">("all");
  let attention = $state(false);
  let refreshing = $state(false);

  async function load() {
    try {
      view = await api.activity(200);
    } catch (e) {
      errorToast(e);
    }
  }

  async function refresh() {
    refreshing = true;
    try {
      await load();
    } finally {
      refreshing = false;
    }
  }

  $effect(() => {
    void revision;
    load();
  });

  const NEEDS_YOU = new Set(["awaiting_approval", "failed", "needs_authentication", "needs_user_action", "outcome_unknown"]);

  const shown = $derived(
    (view?.items ?? [])
      .map((i) =>
        platform === "all" ? i : { ...i, publications: i.publications.filter((p) => p.platform === platform) },
      )
      .filter((i) => platform === "all" || i.publications.length)
      .filter((i) => !attention || i.publications.some((p) => NEEDS_YOU.has(p.state))),
  );
</script>

<section class="page" aria-labelledby="activity-title">
  <div class="page-head">
    <div>
      <h1 id="activity-title">Activity</h1>
      <p class="subtitle">Every video flowing through, newest first, with each destination's status.</p>
    </div>
    <div class="filters" role="group" aria-label="Filter activity">
      <label>Destination
        <select bind:value={platform}>
          <option value="all">All destinations</option>
          {#each PLATFORMS as p (p)}<option value={p}>{platformLabel(p)}</option>{/each}
        </select>
      </label>
      <label class="check" style="flex-direction: row; font-size: var(--text-sm); color: var(--text)">
        <input type="checkbox" bind:checked={attention} /> Only what needs me
      </label>
      <button onclick={refresh} disabled={refreshing} aria-busy={refreshing}>
        {refreshing ? "Refreshing…" : "Refresh"}
      </button>
    </div>
  </div>
  {#if !view}
    <EmptyState title="Loading…" />
  {:else if view.items.length === 0}
    <EmptyState title="No videos yet">
      When a video lands in your watch folder it appears here, then each destination's progress as it is published.
      {#snippet actions()}<button class="primary" onclick={onConnections}>Check your source</button>{/snippet}
    </EmptyState>
  {:else if shown.length === 0}
    <EmptyState title={attention ? "Nothing needs you right now" : "Nothing for this destination yet"} />
  {:else}
    <ul class="items" aria-label="Activity">
      {#each shown as item (item.assetId)}
        <ContentItem {item} onChanged={load} onReconnect={onConnections} {onReview} />
      {/each}
    </ul>
  {/if}
</section>
