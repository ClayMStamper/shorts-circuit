<script lang="ts">
  import type { Tone } from "../../lib/types";

  let { tone = "muted", label }: { tone?: Tone; label: string } = $props();
</script>

<span class="pill {tone}">{label}</span>
