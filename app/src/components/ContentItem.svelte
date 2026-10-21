<script lang="ts" module>
  // Thumbnails are generated on first request; remember them for the session.
  const thumbs = new Map<string, Promise<string | null>>();
</script>

<script lang="ts">
  import { api } from "../lib/api";
  import { approverLabel, formatDate, formatDateTime, formatDuration, timeAgo, toLocalInput } from "../lib/format";
  import { fileUrl } from "../lib/tauri";
  import type { PublicationView, RuleItemView } from "../lib/types";
  import Pill from "./ui/Pill.svelte";
  import PlatformIcon from "./ui/PlatformIcon.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    item,
    onChanged,
    onReconnect,
    onReview,
  }: {
    item: RuleItemView;
    onChanged?: () => void;
    onReconnect?: () => void;
    onReview?: (publicationId: string) => void;
  } = $props();

  let thumb = $state<string | null>(null);
  let busy = $state(false);
  let scheduling = $state<string | null>(null);
  let scheduleAt = $state("");

  $effect(() => {
    const id = item.assetId;
    if (!thumbs.has(id)) thumbs.set(id, api.thumbnail(id).catch(() => null));
    thumbs.get(id)!.then((p) => (thumb = p ? fileUrl(p) : null));
  });

  async function run(fn: () => Promise<unknown>, done?: string) {
    busy = true;
    try {
      await fn();
      if (done) toast(done, "good");
      onChanged?.();
    } catch (e) {
      errorToast(e);
    } finally {
      busy = false;
    }
  }

  function openSchedule(p: PublicationView) {
    const tz = p.timezone ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
    scheduleAt = toLocalInput(p.scheduledAt ?? new Date(Date.now() + 3600_000).toISOString(), tz);
    scheduling = p.id;
  }

  async function saveSchedule(e: SubmitEvent, p: PublicationView) {
    e.preventDefault();
    await run(() => api.schedule(p.id, { kind: "local", local: scheduleAt, timezone: p.timezone }), "Scheduled");
    scheduling = null;
  }

  // Keeps a future posting time; otherwise goes out now.
  function approve(p: PublicationView) {
    const later = p.scheduledAt && new Date(p.scheduledAt).getTime() > Date.now() + 60_000;
    run(() => (later ? api.approve(p.id, p.revision) : api.publishNow(p.id)), "Approved");
  }

  function skip(p: PublicationView) {
    if (!confirm(`Skip this video for ${p.platformName} · ${p.accountName}? You can still publish it later.`)) return;
    run(() => api.cancelPublication(p.id), "Skipped");
  }

  const caption = $derived(item.caption.trim() || item.title);
  const when = $derived(
    item.sourcePostedAt ? `Posted on TikTok ${formatDate(item.sourcePostedAt)}` : `Added ${formatDate(item.addedAt)}`,
  );
</script>

<li class="item" data-testid="content-item">
  <div class="thumb-sm">
    {#if thumb}<img src={thumb} alt="" />{:else}<span aria-hidden="true">▶</span>{/if}
    {#if item.durationMs}<span class="duration">{formatDuration(item.durationMs)}</span>{/if}
  </div>
  <div class="body">
    <p class="caption-text">{caption}</p>
    <span class="when">{when}</span>
    {#if item.waitingForMedia}
      <p class="callout" style="padding: 6px 10px">
        Waiting for the original file: save it to your watch folder. TikTok doesn't let apps download your videos.
      </p>
    {:else if item.waitingReason}
      <p class="muted">{item.waitingReason}</p>
    {/if}
    {#each item.publications as p (p.id)}
      <div class="dest" data-testid="destination">
        <PlatformIcon platform={p.platform} size={22} label={p.platformName} />
        <div class="detail">
          <span><strong>{p.accountName}</strong> · <Pill tone={p.tone} label={p.statusLabel} /></span>
          {#if p.state === "published"}
            <span class="hint">
              {#if p.publishedUrl}
                <button class="link" onclick={() => api.openLink(p.publishedUrl!)}>Open on {p.platformName}</button> ·
              {/if}
              {timeAgo(p.updatedAt)}
            </span>
            {#if p.publishedPrivate}
              <span class="hint">Only you can see it: make it public in YouTube Studio (Content → Visibility).</span>
            {/if}
          {:else if p.progressFraction !== null}
            <span class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(p.progressFraction * 100)} aria-label="Upload progress">
              <span style="width: {Math.round(p.progressFraction * 100)}%"></span>
            </span>
            {#if p.progressText}<span class="hint">{p.progressText}</span>{/if}
          {:else if p.state === "failed" || p.state === "retry_scheduled" || p.state === "needs_authentication"}
            <span class="reason">{p.lastError ?? p.statusText}</span>
          {:else if p.state === "awaiting_approval" && p.blockers.length && p.statusLabel !== "Preparing video"}
            <span class="reason">{p.blockers.join("; ")}</span>
          {:else if p.state === "scheduled" && p.scheduledLocal}
            <span class="hint">Goes out {p.scheduledLocal}</span>
          {:else if p.state === "needs_user_action" || p.state === "outcome_unknown"}
            <span class="hint">{p.statusText}</span>
          {/if}
          {#if p.approvedBy && p.state !== "awaiting_approval"}
            <span class="hint">{approverLabel(p.approvedBy)}</span>
          {/if}
          {#if scheduling === p.id}
            <form class="btn-row" onsubmit={(e) => saveSchedule(e, p)}>
              <label class="sr-only" for="when-{p.id}">Posting time</label>
              <input id="when-{p.id}" type="datetime-local" bind:value={scheduleAt} required />
              <button class="primary small" type="submit" disabled={busy}>Save</button>
              <button class="small" type="button" onclick={() => (scheduling = null)}>Cancel</button>
            </form>
          {/if}
        </div>
        <div class="btn-row">
          {#if p.state === "awaiting_approval"}
            <button class="primary small" disabled={busy || p.statusLabel === "Preparing video"} onclick={() => approve(p)}>
              Approve
            </button>
          {/if}
          {#if p.actions.includes("publishNow") && p.state !== "awaiting_approval"}
            <button class="small" disabled={busy} onclick={() => run(() => api.publishNow(p.id), "Publishing now")}>Publish now</button>
          {/if}
          {#if p.actions.includes("resolveUserAction")}
            <button class="primary small" disabled={busy} onclick={() => run(() => api.resolveUserAction(p.id), "Retrying")}>Retry</button>
          {/if}
          {#if p.actions.includes("retry")}
            <button class="primary small" disabled={busy} onclick={() => run(() => api.retryPublication(p.id), "Retrying")}>Retry</button>
          {/if}
          {#if p.actions.includes("reconnect")}
            <button class="primary small" onclick={() => onReconnect?.()}>Reconnect</button>
          {/if}
          {#if p.actions.includes("republish")}
            <button class="small" disabled={busy} onclick={() => run(() => api.republish(p.id), "Publishing again")}>
              {p.state === "cancelled" ? "Publish anyway" : "Republish"}
            </button>
          {/if}
          {#if p.actions.includes("reschedule") && scheduling !== p.id}
            <button class="small" disabled={busy} onclick={() => openSchedule(p)}>Schedule</button>
          {/if}
          {#if p.actions.includes("cancel") && p.state !== "preparing" && p.state !== "uploading" && p.state !== "processing"}
            <button class="small ghost" disabled={busy} onclick={() => skip(p)}>Skip</button>
          {/if}
          {#if onReview && (p.actions.includes("review") || p.actions.includes("resolveOutcome") || p.actions.includes("confirmPosted"))}
            <button class="small ghost" onclick={() => onReview(p.id)}>Details</button>
          {/if}
        </div>
      </div>
    {:else}
      {#if !item.waitingForMedia && !item.waitingReason}
        <p class="muted">Nothing published from this video{item.addedAt ? ` (added ${formatDateTime(item.addedAt)})` : ""}.</p>
      {/if}
    {/each}
  </div>
</li>
