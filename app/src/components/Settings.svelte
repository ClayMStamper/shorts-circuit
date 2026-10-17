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
  import TikTokSettings from "./TikTokSettings.svelte";

  let { revision = 0 }: { revision?: number } = $props();

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

  async function chooseWatchFolder() {
    const dir = await pickFolder();
    if (dir) update({ watchFolder: dir }, "Watching folder");
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

<section class="settings">
  <h1>Settings</h1>
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
      <legend>Folders</legend>
      <p>
        Watch folder: <code>{prefs.watchFolder ?? "none"}</code>
        <button onclick={chooseWatchFolder}>Choose…</button>
        {#if prefs.watchFolder}<button onclick={() => update({ watchFolder: null }, "Stopped watching")}>Stop watching</button>{/if}
      </p>
      <p class="muted">
        New videos saved here (for example a folder your phone or editor syncs to) are imported automatically once
        they finish writing.
      </p>
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
      <p class="muted">The collection's rules then run for every new import; you still approve each publication.</p>
      <p>
        Export packages go to: <code>{prefs.exportDir ?? "the app's data folder"}</code>
        <button onclick={chooseExportDir}>Choose…</button>
        {#if prefs.exportDir}<button onclick={() => update({ exportDir: null })}>Use default</button>{/if}
      </p>
    </fieldset>

    <TikTokSettings {revision} />

    <fieldset>
      <legend>Destination accounts</legend>
      <ul class="list">
        {#each accounts as a (a.id)}
          <li>
            <strong>{a.platformName}</strong> · {a.displayName}
            <span class="muted">{a.automatic ? "automatic publishing" : `export package — ${a.missingReason ?? ""}`}</span>
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
        Platform sign-in isn't available yet, so every account publishes through export packages: a folder with the
        rendered video, caption and step-by-step posting instructions.
      </p>
    </fieldset>
  {/if}
</section>
