<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { timeAgo } from "../lib/format";
  import type { WorkflowStatus, WorkflowView, WorkflowsView } from "../lib/types";
  import CreateWorkflow from "./CreateWorkflow.svelte";
  import SetupWizard from "./SetupWizard.svelte";
  import EmptyState from "./ui/EmptyState.svelte";
  import OverflowMenu from "./ui/OverflowMenu.svelte";
  import Pill from "./ui/Pill.svelte";
  import PlatformIcon from "./ui/PlatformIcon.svelte";
  import Switch from "./ui/Switch.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    revision = 0,
    onOpen,
    onConnections,
  }: {
    revision?: number;
    onOpen: (id: string, settings?: boolean) => void;
    onConnections: () => void;
  } = $props();

  let view = $state<WorkflowsView | null>(null);
  let wizard = $state(false);
  let creating = $state(false);
  let checking = $state(false);
  let renaming = $state<string | null>(null);
  let newName = $state("");
  let loadError = $state<string | null>(null);

  async function load() {
    try {
      view = await api.listWorkflows();
      loadError = null;
    } catch (e) {
      loadError = (e as { message: string }).message;
    }
  }

  onMount(load);
  $effect(() => {
    void revision;
    load();
  });

  const showWizard = $derived(wizard || (view !== null && view.workflows.length === 0));

  async function update(w: WorkflowView, change: Parameters<typeof api.updateWorkflow>[1], message?: string) {
    try {
      await api.updateWorkflow(w.id, change);
      if (message) toast(message, "good");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  async function checkNow() {
    checking = true;
    try {
      view = await api.checkForNewContent();
      toast("Checked for new content", "good");
    } catch (e) {
      errorToast(e);
    } finally {
      checking = false;
    }
  }

  async function remove(w: WorkflowView) {
    if (!confirm(`Delete "${w.name}"? Nothing already published is removed; new videos won't go to ${w.platformName}.`)) return;
    try {
      await api.deleteWorkflow(w.id);
      toast("Workflow deleted", "good");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  function startRename(w: WorkflowView) {
    renaming = w.id;
    newName = w.name;
  }

  async function saveRename(e: SubmitEvent, w: WorkflowView) {
    e.preventDefault();
    renaming = null;
    if (newName.trim() && newName.trim() !== w.name) await update(w, { name: newName.trim() }, "Renamed");
  }

  const STATUS: Record<WorkflowStatus, { label: string; tone: "good" | "muted" | "bad" }> = {
    active: { label: "Active", tone: "good" },
    inactive: { label: "Inactive", tone: "muted" },
    needs_reconnect: { label: "Needs reconnect", tone: "bad" },
  };
</script>

<section class="page" aria-labelledby="workflows-title">
  {#if loadError}
    <p class="callout bad" role="alert">Couldn't load your workflows: {loadError}</p>
  {/if}
  {#if view && showWizard}
    <SetupWizard
      onDone={async () => {
        wizard = false;
        await load();
      }}
      onCancel={view.workflows.length ? () => (wizard = false) : undefined}
    />
  {:else if view}
    <div class="page-head">
      <div>
        <h1 id="workflows-title">Workflows</h1>
        <p class="subtitle">Every new video from your source goes to each destination below.</p>
      </div>
      <div class="btn-row">
        <button onclick={() => (wizard = true)}>Setup guide</button>
        <button onclick={checkNow} disabled={checking}>{checking ? "Checking…" : "Check for new content now"}</button>
        <button class="primary" onclick={() => (creating = true)}>Create workflow</button>
      </div>
    </div>

    <div class="source-bar" data-testid="source-bar">
      <PlatformIcon platform="tiktok" size={20} />
      {#if view.source.watchFolder}
        <span>Watching <code>{view.source.watchFolder}</code></span>
        <span>· {view.source.watchCheckedAt ? `Last checked ${timeAgo(view.source.watchCheckedAt)}` : "Not checked yet"}</span>
      {:else}
        <span class="warn">No watch folder yet: videos can't arrive.</span>
        <button class="link" onclick={onConnections}>Choose one in Connections</button>
      {/if}
      <span>· TikTok captions {view.source.tiktokConnected ? "on" : "off"}</span>
    </div>

    <ul class="workflow-list" aria-label="Workflows">
      {#each view.workflows as w (w.id)}
        <li class="workflow-row" data-testid="workflow-row">
          <span class="flow" aria-hidden="true">
            <PlatformIcon platform="tiktok" />→<PlatformIcon platform={w.platform} />
          </span>
          <div class="name">
            {#if renaming === w.id}
              <form onsubmit={(e) => saveRename(e, w)}>
                <label class="sr-only" for="rename-{w.id}">Workflow name</label>
                <!-- svelte-ignore a11y_autofocus -->
                <input id="rename-{w.id}" bind:value={newName} autofocus onblur={() => (renaming = null)} />
              </form>
            {:else}
              <strong>
                <button class="link" style="color: inherit; font-weight: 600" onclick={() => onOpen(w.id)}>{w.name}</button>
              </strong>
            {/if}
            <span class="account">＠{w.sourceName} → ＠{w.accountName}</span>
          </div>
          <span title={w.autoPublishAvailable ? "" : `Connect ${w.accountName} to publish automatically`}>
            <Switch
              checked={w.autoPublish}
              label="Auto Publish"
              disabled={!w.autoPublishAvailable}
              onchange={(on) => update(w, { autoPublish: on }, on ? "Auto Publish on" : "Auto Publish off: new videos wait for your review")}
            />
          </span>
          <div class="status-cell">
            <Pill tone={STATUS[w.status].tone} label={STATUS[w.status].label} />
            {#if w.status === "inactive"}
              <button class="link small" onclick={() => update(w, { enabled: true }, "Reactivated")}>Reactivate</button>
            {:else if w.status === "needs_reconnect"}
              <button class="link small" onclick={onConnections}>Reconnect</button>
            {/if}
          </div>
          <span class="count">{w.itemCount} {w.itemCount === 1 ? "item" : "items"}</span>
          <div class="btn-row">
            <button onclick={() => onOpen(w.id)}>View</button>
            <OverflowMenu
              label="More for {w.name}"
              items={[
                { label: "Rename", run: () => startRename(w) },
                w.enabled
                  ? { label: "Pause", run: () => update(w, { enabled: false }, "Paused") }
                  : { label: "Reactivate", run: () => update(w, { enabled: true }, "Reactivated") },
                { label: "Advanced settings", run: () => onOpen(w.id, true) },
                { label: "Delete", danger: true, run: () => remove(w) },
              ]}
            />
          </div>
        </li>
      {/each}
    </ul>

    {#if view.suggestions.length}
      <div class="box">
        <h2 style="font-size: var(--text-lg)">Also connected</h2>
        {#each view.suggestions as s (s.accountId)}
          <div class="suggestion">
            <span class="flow"><PlatformIcon platform="tiktok" />→<PlatformIcon platform={s.platform} /></span>
            <span><strong>{s.name}</strong> <span class="muted">· {s.accountName}</span></span>
            <button onclick={() => (creating = true)}>Add workflow</button>
          </div>
        {/each}
      </div>
    {/if}
  {:else if !loadError}
    <EmptyState title="Loading your workflows…" />
  {/if}
</section>

{#if creating && view}
  <CreateWorkflow
    suggestions={view.suggestions}
    onclose={() => (creating = false)}
    onConnections={() => {
      creating = false;
      onConnections();
    }}
    onCreated={async () => {
      creating = false;
      await load();
    }}
  />
{/if}
