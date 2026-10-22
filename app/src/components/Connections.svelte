<script lang="ts">
  import { PLATFORMS } from "../lib/types";
  import CloudSettings from "./CloudSettings.svelte";
  import DestinationCard from "./DestinationCard.svelte";
  import SourceCard from "./SourceCard.svelte";

  let { revision = 0, onChanged }: { revision?: number; onChanged?: () => void } = $props();
</script>

<section class="page" aria-labelledby="connections-title">
  <div class="page-head">
    <div>
      <h1 id="connections-title">Connections</h1>
      <p class="subtitle">Set each one up once. Keys and tokens stay in your system's credential store.</p>
    </div>
  </div>
  <h2>Source</h2>
  <SourceCard {revision} />
  <h2>Destinations</h2>
  <div class="cards">
    {#each PLATFORMS as p (p)}
      <DestinationCard platform={p} {revision} {onChanged} />
    {/each}
  </div>
  <details class="box">
    <summary><strong>Cloud runner (optional)</strong>: keep posting while this computer is off</summary>
    <CloudSettings {revision} />
  </details>
</section>
