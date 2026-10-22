<script lang="ts">
  import type { Snippet } from "svelte";

  let { title, children, actions }: { title: string; children?: Snippet; actions?: Snippet } = $props();
</script>

<div class="empty-state">
  <h2>{title}</h2>
  {#if children}<p>{@render children()}</p>{/if}
  {#if actions}<div class="btn-row" style="justify-content: center">{@render actions()}</div>{/if}
</div>
