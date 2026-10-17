<script lang="ts">
  import { stateLabel, stateTone } from "../lib/format";
  import type { PublicationState } from "../lib/types";

  let { state }: { state: PublicationState } = $props();
</script>

<span class="badge {stateTone(state)}">{stateLabel(state)}</span>
