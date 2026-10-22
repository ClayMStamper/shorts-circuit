<script lang="ts">
  // An on/off switch announced as such to screen readers (role="switch").
  let {
    checked,
    label,
    showLabel = true,
    disabled = false,
    onchange,
  }: {
    checked: boolean;
    label: string;
    showLabel?: boolean;
    disabled?: boolean;
    onchange: (next: boolean) => void;
  } = $props();
</script>

<button
  type="button"
  class="switch"
  role="switch"
  aria-checked={checked}
  aria-label={showLabel ? undefined : label}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="track" aria-hidden="true"></span>
  {#if showLabel}<span>{label}</span>{/if}
</button>
