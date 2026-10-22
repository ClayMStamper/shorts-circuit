<script lang="ts" module>
  export interface MenuItem {
    label: string;
    danger?: boolean;
    run: () => void;
  }
</script>

<script lang="ts">
  // A "⋯" button with a small menu. Escape or clicking elsewhere closes it; focus moves into
  // the menu when it opens and back to the button when it closes.
  import { tick } from "svelte";

  let { label, items }: { label: string; items: MenuItem[] } = $props();
  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();
  let trigger: HTMLButtonElement | undefined = $state();

  async function toggle() {
    open = !open;
    if (open) {
      await tick();
      root?.querySelector<HTMLButtonElement>("ul button")?.focus();
    }
  }

  function close(refocus = true) {
    open = false;
    if (refocus) trigger?.focus();
  }

  function onKey(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const buttons = Array.from(root?.querySelectorAll<HTMLButtonElement>("ul button") ?? []);
      const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const next = e.key === "ArrowDown" ? (i + 1) % buttons.length : (i - 1 + buttons.length) % buttons.length;
      buttons[next]?.focus();
    }
  }

  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) close(false);
  }
</script>

<svelte:window onclick={onWindowClick} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="menu" bind:this={root} onkeydown={onKey}>
  <button
    type="button"
    class="ghost"
    bind:this={trigger}
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label={label}
    onclick={toggle}>⋯</button
  >
  {#if open}
    <ul role="menu" aria-label={label}>
      {#each items as item (item.label)}
        <li role="none">
          <button
            type="button"
            role="menuitem"
            class:danger={item.danger}
            onclick={() => {
              close();
              item.run();
            }}>{item.label}</button
          >
        </li>
      {/each}
    </ul>
  {/if}
</div>
