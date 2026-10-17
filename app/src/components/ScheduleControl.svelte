<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatInZone, systemTimeZone, toLocalInput } from "../lib/format";
  import type { ScheduleInput } from "../lib/types";

  let {
    value = $bindable(null),
    accountId,
    initialUtc = null,
    initialZone = null,
  }: {
    value?: ScheduleInput | null;
    accountId: string | null;
    initialUtc?: string | null;
    initialZone?: string | null;
  } = $props();

  let timezone = $state(systemTimeZone());
  let local = $state("");
  let slot = $state<string | null>(null);
  let mode = $state<"none" | "local" | "now" | "nextSlot">("none");
  let zones = $state<string[]>([]);

  onMount(async () => {
    try {
      const prefs = await api.preferences();
      timezone = initialZone ?? prefs.timezone;
    } catch {
      timezone = initialZone ?? systemTimeZone();
    }
    if (initialUtc) {
      local = toLocalInput(initialUtc, timezone);
      mode = "local";
    }
    try {
      zones = Intl.supportedValuesOf("timeZone");
    } catch {
      zones = [timezone];
    }
    if (!zones.includes(timezone)) zones = [timezone, ...zones];
  });

  $effect(() => {
    if (mode === "local" && local) value = { kind: "local", local, timezone };
    else if (mode === "now") value = { kind: "now" };
    else if (mode === "nextSlot") value = { kind: "nextSlot" };
    else value = null;
  });

  async function nextSlot() {
    if (!accountId) return;
    slot = await api.nextSlot(accountId);
    if (slot) {
      local = toLocalInput(slot, timezone);
      mode = "local";
    } else {
      mode = "nextSlot";
    }
  }

  function now() {
    local = "";
    slot = null;
    mode = "now";
  }
</script>

<div class="schedule-control">
  <label>
    Posting time
    <input type="datetime-local" bind:value={local} oninput={() => (mode = "local")} />
  </label>
  <label>
    Timezone
    <select bind:value={timezone}>
      {#each zones as z (z)}<option value={z}>{z}</option>{/each}
    </select>
  </label>
  <button type="button" disabled={!accountId} onclick={nextSlot}>Next free slot</button>
  <button type="button" onclick={now}>As soon as possible</button>
  {#if mode === "now"}
    <span class="muted">Goes out right after approval.</span>
  {:else if mode === "nextSlot"}
    <span class="muted">No posting slots configured; add some in Settings.</span>
  {:else if slot}
    <span class="muted">Next slot: {formatInZone(slot, timezone)}</span>
  {/if}
</div>
