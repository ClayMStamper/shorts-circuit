<script lang="ts">
  let {
    value,
    onChange,
  }: { value: number | null; onChange?: (stars: number) => void } = $props();
</script>

<span class="stars" role="group" aria-label="Rating">
  {#each [1, 2, 3, 4, 5] as n (n)}
    <button
      type="button"
      class:on={(value ?? 0) >= n}
      disabled={!onChange}
      aria-label={`${n} star${n > 1 ? "s" : ""}`}
      onclick={() => onChange?.(value === n ? 0 : n)}>★</button
    >
  {/each}
</span>
