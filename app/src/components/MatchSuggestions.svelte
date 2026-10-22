<script lang="ts">
  import { api } from "../lib/api";
  import { formatDateTime, formatDuration } from "../lib/format";
  import type { MatchSuggestion } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision, onOpen }: { revision: number; onOpen: (id: string) => void } = $props();

  let suggestions = $state<MatchSuggestion[]>([]);

  async function load() {
    try {
      suggestions = await api.matchSuggestions();
    } catch (e) {
      errorToast(e);
    }
  }

  $effect(() => {
    void revision;
    load();
  });

  async function confirm(s: MatchSuggestion) {
    try {
      const merged = await api.confirmMatch(s.post.videoId, s.candidate.assetId);
      toast(`Matched "${merged.title}" with ${s.candidate.originalName}`, "good");
      await load();
      onOpen(merged.id);
    } catch (e) {
      errorToast(e);
    }
  }

  async function dismiss(s: MatchSuggestion) {
    try {
      await api.dismissMatch(s.post.videoId, s.candidate.assetId);
      await load();
    } catch (e) {
      errorToast(e);
    }
  }
</script>

{#if suggestions.length > 0}
  <section class="panel matches" aria-label="Possible TikTok matches">
    <h2>Is this the original?</h2>
    <ul class="list">
      {#each suggestions as s (`${s.post.videoId}:${s.candidate.assetId}`)}
        <li data-testid="match">
          <span>
            TikTok post <strong>{s.post.title}</strong>
            <span class="muted">
              (posted {formatDateTime(s.post.postedAt)}{#if s.post.durationMs !== null}, {formatDuration(s.post.durationMs)}{/if})
            </span>
            ↔ file <strong>{s.candidate.originalName}</strong>
            <span class="muted">
              (arrived {formatDateTime(s.candidate.importedAt)}, {formatDuration(s.candidate.durationMs)})
            </span>
          </span>
          <button class="primary" onclick={() => confirm(s)}>Yes, same video</button>
          <button onclick={() => dismiss(s)}>Not a match</button>
        </li>
      {/each}
    </ul>
    <p class="muted">
      Confirming gives the file the post's title, caption and link, adds it to the post's collections, and removes the
      placeholder item.
    </p>
  </section>
{/if}
