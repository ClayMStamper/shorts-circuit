<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { timeAgo } from "../lib/format";
  import type { RuleItemView, WorkflowDetailView, WorkflowsView } from "../lib/types";
  import ContentItem from "./ContentItem.svelte";
  import WorkflowSettings from "./WorkflowSettings.svelte";
  import EmptyState from "./ui/EmptyState.svelte";
  import Pill from "./ui/Pill.svelte";
  import PlatformIcon from "./ui/PlatformIcon.svelte";
  import Switch from "./ui/Switch.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    id,
    revision = 0,
    openSettings = false,
    onBack,
    onConnections,
    onReview,
  }: {
    id: string;
    revision?: number;
    openSettings?: boolean;
    onBack: () => void;
    onConnections: () => void;
    onReview?: (publicationId: string) => void;
  } = $props();

  let detail = $state<WorkflowDetailView | null>(null);
  let source = $state<WorkflowsView["source"] | null>(null);
  let settings = $state(false);
  let checking = $state(false);
  let sort = $state<"newest" | "oldest">("newest");
  let duration = $state<"all" | "short" | "medium" | "long">("all");
  let status = $state<"all" | "published" | "processing" | "waiting" | "failed">("all");

  onMount(() => {
    settings = openSettings;
  });

  async function load() {
    try {
      const [d, w] = await Promise.all([api.workflowDetail(id), api.listWorkflows()]);
      detail = d;
      source = w.source;
    } catch (e) {
      errorToast(e);
    }
  }

  $effect(() => {
    void revision;
    void id;
    load();
  });

  async function setAuto(on: boolean) {
    if (!detail) return;
    try {
      detail.workflow = await api.updateWorkflow(id, { autoPublish: on });
      toast(on ? "Auto Publish on" : "Auto Publish off: new videos wait for your review", "good");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  async function reactivate() {
    try {
      await api.updateWorkflow(id, { enabled: true });
      toast("Reactivated", "good");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  async function checkNow() {
    checking = true;
    try {
      source = (await api.checkForNewContent()).source;
      await load();
    } catch (e) {
      errorToast(e);
    } finally {
      checking = false;
    }
  }

  function bucket(item: RuleItemView): "published" | "processing" | "waiting" | "failed" {
    const states = item.publications.map((p) => p.state);
    if (states.some((s) => s === "failed" || s === "needs_authentication" || s === "outcome_unknown")) return "failed";
    if (states.some((s) => s === "awaiting_approval" || s === "needs_user_action" || s === "draft")) return "waiting";
    if (states.length && states.every((s) => s === "published" || s === "cancelled")) return "published";
    return item.publications.length ? "processing" : "waiting";
  }

  const shown = $derived.by(() => {
    if (!detail) return [];
    let items = detail.items.filter((i) => {
      const ms = i.durationMs ?? 0;
      if (duration === "short" && ms >= 30_000) return false;
      if (duration === "medium" && (ms < 30_000 || ms > 60_000)) return false;
      if (duration === "long" && ms <= 60_000) return false;
      return status === "all" || bucket(i) === status;
    });
    items = [...items].sort((a, b) => a.addedAt.localeCompare(b.addedAt));
    return sort === "newest" ? items.reverse() : items;
  });

  const tone = $derived(
    !detail
      ? "info"
      : detail.workflow.status === "needs_reconnect"
        ? "bad"
        : !detail.workflow.enabled
          ? "warn"
          : detail.workflow.autoPublish
            ? "good"
            : "warn",
  );
</script>

<section class="page" aria-labelledby="wf-title">
  <nav class="crumbs" aria-label="Breadcrumb">
    <button class="link" onclick={onBack}>Workflows</button><span aria-hidden="true">›</span>
    <span aria-current="page">{detail?.workflow.name ?? "…"}</span>
  </nav>
  {#if detail}
    {@const w = detail.workflow}
    <div class="wf-head">
      <h1 id="wf-title">
        <PlatformIcon platform="tiktok" label="TikTok" /> ＠{w.sourceName}
        <span aria-hidden="true">→</span>
        <PlatformIcon platform={w.platform} label={w.platformName} /> ＠{w.accountName}
      </h1>
      <Pill
        tone={w.status === "active" ? "good" : w.status === "inactive" ? "muted" : "bad"}
        label={w.status === "active" ? "Active" : w.status === "inactive" ? "Inactive" : "Needs reconnect"}
      />
      <span style="flex: 1"></span>
      <Switch checked={w.autoPublish} label="Auto Publish" disabled={!w.autoPublishAvailable} onchange={setAuto} />
      <button onclick={() => (settings = true)}>Settings</button>
    </div>

    <div class="callout {tone}" data-testid="workflow-banner">
      <p>{w.banner}</p>
      {#if w.status === "inactive"}
        <button class="primary small" onclick={reactivate}>Reactivate</button>
      {:else if w.status === "needs_reconnect"}
        <button class="primary small" onclick={onConnections}>Reconnect</button>
      {/if}
    </div>

    <div class="source-bar">
      {#if source?.watchFolder}
        <span>{source.watchCheckedAt ? `Last checked ${timeAgo(source.watchCheckedAt)}` : "Not checked yet"}</span>
        ·
      {/if}
      <button class="link" onclick={checkNow} disabled={checking}>{checking ? "Checking…" : "Check now"}</button>
    </div>

    <div class="filters" role="group" aria-label="Filter content">
      <label>Sort
        <select bind:value={sort}><option value="newest">Newest first</option><option value="oldest">Oldest first</option></select>
      </label>
      <label>Duration
        <select bind:value={duration}>
          <option value="all">Any length</option>
          <option value="short">Under 30 s</option>
          <option value="medium">30–60 s</option>
          <option value="long">Over 60 s</option>
        </select>
      </label>
      <label>Status
        <select bind:value={status}>
          <option value="all">All</option>
          <option value="published">Published</option>
          <option value="processing">In progress</option>
          <option value="waiting">Waiting for you</option>
          <option value="failed">Needs attention</option>
        </select>
      </label>
      <span class="muted" aria-live="polite">{shown.length} of {detail.items.length}</span>
    </div>

    {#if detail.items.length === 0}
      <EmptyState title="Nothing here yet">
        {#if source?.watchFolder}
          Save a video to <code>{source.watchFolder}</code>; it shows up here within a minute and
          {w.autoPublish ? "goes out automatically" : "waits for your review"}.
        {:else}
          Choose the folder your videos arrive in (Connections → Source), then save a video there.
        {/if}
        {#snippet actions()}
          {#if !source?.watchFolder}<button class="primary" onclick={onConnections}>Choose a folder</button>{/if}
        {/snippet}
      </EmptyState>
    {:else if shown.length === 0}
      <EmptyState title="No content matches these filters" />
    {:else}
      <ul class="items" aria-label="Content">
        {#each shown as item (item.assetId)}
          <ContentItem {item} onChanged={load} onReconnect={onConnections} {onReview} />
        {/each}
      </ul>
    {/if}

    {#if settings}
      <WorkflowSettings
        workflow={w}
        onclose={() => (settings = false)}
        onSaved={async () => {
          settings = false;
          await load();
        }}
      />
    {/if}
  {:else}
    <EmptyState title="Loading…" />
  {/if}
</section>
