<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { platformLabel } from "../lib/format";
  import { pickFolder } from "../lib/tauri";
  import {
    PLATFORMS,
    WEEKDAYS,
    type AccountView,
    type CollectionView,
    type Platform,
    type Preferences,
    type PostingSlot,
    type Weekday,
  } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  type Advanced = "library" | "collections" | "monitor";
  let { revision = 0, onAdvanced }: { revision?: number; onAdvanced?: (to: Advanced) => void } = $props();

  let prefs = $state<Preferences | null>(null);
  let accounts = $state<AccountView[]>([]);
  let collections = $state<CollectionView[]>([]);
  let zones = $state<string[]>([]);
  let graceMinutes = $state(15);
  let newPlatform = $state<Platform>("youtube_shorts");
  let newName = $state("");
  let slotDays = $state<Weekday[]>([]);
  let slotTime = $state("18:00");

  onMount(async () => {
    try {
      zones = Intl.supportedValuesOf("timeZone");
    } catch {
      zones = [];
    }
    await load();
  });

  async function load() {
    try {
      [prefs, accounts, collections] = await Promise.all([
        api.preferences(),
        api.listAccounts(),
        api.listCollections(),
      ]);
      if (prefs.overduePolicy.kind === "expire") graceMinutes = Math.round(prefs.overduePolicy.grace_secs / 60);
      if (!zones.includes(prefs.timezone)) zones = [prefs.timezone, ...zones];
    } catch (e) {
      errorToast(e);
    }
  }

  async function update(update: Parameters<typeof api.updatePreferences>[0], message = "Saved") {
    try {
      prefs = await api.updatePreferences(update);
      toast(message, "good");
    } catch (e) {
      errorToast(e);
    }
  }

  async function chooseExportDir() {
    const dir = await pickFolder();
    if (dir) update({ exportDir: dir });
  }

  function addSlot() {
    if (!prefs) return;
    const slot: PostingSlot = { weekdays: [...slotDays], time: `${slotTime}:00` };
    update({ postingSlots: [...prefs.postingSlots, slot] });
  }

  function removeSlot(index: number) {
    if (!prefs) return;
    update({ postingSlots: prefs.postingSlots.filter((_, i) => i !== index) });
  }

  async function addAccount(event: SubmitEvent) {
    event.preventDefault();
    try {
      await api.addAccount(newPlatform, newName);
      newName = "";
      accounts = await api.listAccounts();
    } catch (e) {
      errorToast(e);
    }
  }
</script>

<section class="settings page">
  <div class="page-head">
    <div>
      <h1>Settings</h1>
      <p class="subtitle">Rarely used options. Accounts and the watch folder are in Connections.</p>
    </div>
  </div>
  {#if prefs}
    <fieldset>
      <legend>Time</legend>
      <label>
        Your timezone
        <select value={prefs.timezone} onchange={(e) => update({ timezone: e.currentTarget.value })}>
          {#each zones as z (z)}<option value={z}>{z}</option>{/each}
        </select>
      </label>
      <p class="muted">Posting times are stored in UTC and shown in this timezone; posting slots follow its daylight-saving changes.</p>
    </fieldset>

    <fieldset>
      <legend>If the app was closed at a posting time</legend>
      <label class="check">
        <input
          type="radio"
          name="overdue"
          checked={prefs.overduePolicy.kind === "expire"}
          onchange={() => update({ overduePolicy: { kind: "expire", grace_secs: graceMinutes * 60 } })}
        />
        Ask me if it is more than
        <input
          type="number"
          min="0"
          class="narrow"
          bind:value={graceMinutes}
          onchange={() => update({ overduePolicy: { kind: "expire", grace_secs: graceMinutes * 60 } })}
        />
        minutes late
      </label>
      <label class="check">
        <input
          type="radio"
          name="overdue"
          checked={prefs.overduePolicy.kind === "catch_up"}
          onchange={() => update({ overduePolicy: { kind: "catch_up" } })}
        />
        Post it as soon as the app is running again
      </label>
    </fieldset>

    <fieldset>
      <legend>Posting slots</legend>
      <ul class="list">
        {#each prefs.postingSlots as s, i (i)}
          <li>
            {s.weekdays.length ? s.weekdays.join(", ") : "Every day"} at {s.time.slice(0, 5)}
            <button onclick={() => removeSlot(i)}>Remove</button>
          </li>
        {/each}
      </ul>
      <div class="row">
        {#each WEEKDAYS as d (d)}
          <label class="check"><input type="checkbox" value={d} bind:group={slotDays} /> {d}</label>
        {/each}
        <input type="time" bind:value={slotTime} />
        <button onclick={addSlot}>Add slot</button>
      </div>
      <p class="muted">"Next free slot" picks the next of these times that has no other post to the same account within 30 minutes.</p>
    </fieldset>

    <fieldset>
      <legend>Export packages</legend>
      <p>
        Export packages go to: <code>{prefs.exportDir ?? "the app's data folder"}</code>
        <button onclick={chooseExportDir}>Choose…</button>
        {#if prefs.exportDir}<button onclick={() => update({ exportDir: null })}>Use default</button>{/if}
      </p>
      <p class="muted">For destinations without a connection: a folder with the video, caption and posting steps.</p>
    </fieldset>

    <fieldset>
      <legend>Destination accounts</legend>
      <ul class="list">
        {#each accounts as a (a.id)}
          <li>
            <strong>{a.platformName}</strong> · {a.displayName}
            <span class="muted">
              {#if a.automatic}
                uploads automatically{a.privateOnly ? " (as private until the API audit)" : ""}
              {:else}
                export package — {a.missingReason ?? ""}
              {/if}
            </span>
          </li>
        {/each}
      </ul>
      <form class="row" onsubmit={addAccount}>
        <select bind:value={newPlatform}>
          {#each PLATFORMS as p (p)}<option value={p}>{platformLabel(p)}</option>{/each}
        </select>
        <input placeholder="Account name" bind:value={newName} required />
        <button type="submit">Add account</button>
      </form>
      <p class="muted">
        Connect YouTube channels, Instagram accounts, LinkedIn pages and Snapchat profiles in Connections to publish
        automatically. Accounts added here publish through export packages instead.
      </p>
    </fieldset>

    <fieldset>
      <legend>Advanced</legend>
      <p class="muted">
        The building blocks under workflows. You don't need these day to day: workflows render, caption and publish on
        their own.
      </p>
      <div class="advanced-nav">
        <button onclick={() => onAdvanced?.("library")}>Library</button>
        <button onclick={() => onAdvanced?.("collections")}>Collections and rules</button>
        <button onclick={() => onAdvanced?.("monitor")}>Monitor and jobs</button>
      </div>
      <label>
        Put watch-folder imports in
        <select
          value={prefs.watchCollection ?? ""}
          onchange={(e) => update({ watchCollection: e.currentTarget.value || null })}
        >
          <option value="">no collection</option>
          {#each collections as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
        </select>
      </label>
      <p class="muted">Workflows use this collection (created as "Source videos"). Changing it moves where new videos go.</p>
    </fieldset>
  {/if}
</section>
