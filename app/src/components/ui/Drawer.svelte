<script lang="ts">
  // A side panel for settings. It is a modal dialog: focus moves into it, Escape closes it and
  // focus returns to whatever opened it.
  import { onMount, type Snippet } from "svelte";

  let {
    title,
    onclose,
    children,
    footer,
  }: { title: string; onclose: () => void; children: Snippet; footer?: Snippet } = $props();
  let panel: HTMLElement | undefined = $state();
  const opener = typeof document !== "undefined" ? (document.activeElement as HTMLElement | null) : null;

  onMount(() => {
    panel?.querySelector<HTMLElement>("input, select, textarea, button")?.focus();
    return () => opener?.focus?.();
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    }
  }
</script>

<div class="drawer-backdrop" role="presentation" onclick={onclose}></div>
<div class="drawer" role="dialog" aria-modal="true" aria-labelledby="drawer-title" bind:this={panel} onkeydown={onKey} tabindex="-1">
  <header>
    <h2 id="drawer-title">{title}</h2>
    <button type="button" class="ghost" aria-label="Close" onclick={onclose}>✕</button>
  </header>
  <div class="drawer-body">{@render children()}</div>
  {#if footer}<footer>{@render footer()}</footer>{/if}
</div>
