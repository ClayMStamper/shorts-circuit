<script lang="ts">
  import { api } from "../lib/api";
  import { formatBytes, formatDuration, formatInZone, parseHashtags } from "../lib/format";
  import { CAPTION_LIMITS, captionLength } from "../lib/limits";
  import { fileUrl } from "../lib/tauri";
  import type { ReviewView, ScheduleInput } from "../lib/types";
  import Badge from "./Badge.svelte";
  import ScheduleControl from "./ScheduleControl.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let { id, revision, onBack }: { id: string; revision: number; onBack: () => void } = $props();

  let review = $state<ReviewView | null>(null);
  let editing = $state(false);
  let caption = $state("");
  let tagsText = $state("");
  let rescheduling = $state(false);
  let schedule = $state<ScheduleInput | null>(null);

  async function load() {
    try {
      review = await api.review(id);
    } catch (e) {
      errorToast(e);
    }
  }

  $effect(() => {
    void [id, revision];
    load();
  });

  async function run(action: () => Promise<unknown>, message?: string) {
    try {
      await action();
      if (message) toast(message, "good");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  function startEdit() {
    if (!review) return;
    caption = review.publication.caption;
    tagsText = review.publication.hashtags.map((t) => `#${t}`).join(" ");
    editing = true;
  }

  async function saveEdit() {
    await run(
      () => api.editPublication(id, { caption, hashtags: parseHashtags(tagsText) }),
      "Saved as a new revision; it needs approval again.",
    );
    editing = false;
  }

  async function saveSchedule() {
    if (!schedule) return;
    await run(() => api.schedule(id, schedule!), "Posting time updated.");
    rescheduling = false;
  }
</script>

<section class="review">
  <button class="link" onclick={onBack}>← Monitor</button>
  {#if review}
    {@const p = review.publication}
    {@const limit = CAPTION_LIMITS[p.platform]}
    <header class="review-head">
      <h1>{p.assetTitle}</h1>
      <Badge state={p.state} />
      <span class="muted">revision {p.revision}{p.approved ? " · approved" : ""}</span>
    </header>
    <p class="status">{p.statusText}</p>

    <div class="review-grid">
      <div class="video-col">
        {#if review.videoPath}
          <!-- svelte-ignore a11y_media_has_caption -->
          <video class="preview vertical" src={fileUrl(review.videoPath)} controls preload="metadata" data-testid="review-video"></video>
          {#if review.variant.rendered}
            <p class="muted">
              {review.variant.name} · {review.variant.rendered.properties.width}x{review.variant.rendered.properties.height} ·
              {formatDuration(review.variant.rendered.properties.duration_ms)} · {formatBytes(review.variant.rendered.sizeBytes)}
            </p>
          {/if}
        {:else}
          <div class="placeholder-box vertical">
            <p>The final video hasn't been rendered yet.</p>
            <button disabled={review.variant.rendering} onclick={() => run(() => api.renderVariant(review!.variant.id), "Rendering…")}>
              {review.variant.rendering ? "Rendering…" : "Render now"}
            </button>
          </div>
        {/if}
      </div>

      <div class="facts">
        <section>
          <h3>Destination</h3>
          <p><strong>{review.account.platformName}</strong> · {review.account.displayName}</p>
          {#if !review.account.automatic}
            <p class="muted">Published through an export package: {review.account.missingReason}</p>
          {/if}
        </section>

        <section>
          <h3>Copy</h3>
          {#if editing}
            <textarea rows="5" bind:value={caption}></textarea>
            <input bind:value={tagsText} placeholder="#hashtags" />
            <p class="muted">{captionLength(caption, parseHashtags(tagsText))}/{limit.chars} characters</p>
            <button class="primary" onclick={saveEdit}>Save copy</button>
            <button onclick={() => (editing = false)}>Cancel</button>
          {:else}
            <pre class="caption" data-testid="caption">{review.fullCaption}</pre>
            <p class="muted">{[...review.fullCaption].length}/{limit.chars} characters</p>
            {#if p.actions.includes("edit")}<button onclick={startEdit}>Edit copy</button>{/if}
          {/if}
        </section>

        <section>
          <h3>Time</h3>
          {#if p.scheduledAt && p.timezone}
            <p data-testid="time"><strong>{formatInZone(p.scheduledAt, p.timezone)}</strong></p>
            <p class="muted">{p.scheduledAt.replace("T", " ").slice(0, 16)} UTC</p>
          {:else}
            <p class="error">No posting time set.</p>
          {/if}
          {#if rescheduling}
            <ScheduleControl bind:value={schedule} accountId={p.accountId} initialUtc={p.scheduledAt} initialZone={p.timezone} />
            <button class="primary" disabled={!schedule} onclick={saveSchedule}>Set time</button>
            <button onclick={() => (rescheduling = false)}>Cancel</button>
          {:else if p.actions.includes("reschedule")}
            <button onclick={() => (rescheduling = true)}>Change time</button>
          {/if}
        </section>

        {#if review.issueText.length > 0 || review.warnings.length > 0}
          <section class="issues">
            <h3>{p.state === "awaiting_approval" ? "Check before approving" : "Checks"}</h3>
            <ul>
              {#each review.issueText as i (i)}<li class="error">{i}</li>{/each}
              {#each review.warnings as w (w)}<li class="warn">{w}</li>{/each}
            </ul>
            <p class="muted">Platform limits are indicative and are re-checked when publishing.</p>
          </section>
        {/if}

        <div class="actions">
          {#if p.state === "draft"}
            <button class="primary" onclick={() => run(() => api.submitForReview(id))}>Submit for review</button>
          {/if}
          {#if p.state === "awaiting_approval"}
            <button
              class="primary"
              disabled={!review.canApprove}
              title={review.canApprove ? "" : "Render the video and set a time first"}
              onclick={() => run(() => api.approve(id, p.revision), "Approved and scheduled.")}
            >
              Approve revision {p.revision}
            </button>
          {/if}
          {#if p.actions.includes("publishNow")}
            <button onclick={() => run(() => api.schedule(id, { kind: "now" }), "Going out now.")}>Post now</button>
          {/if}
          {#if p.actions.includes("revokeApproval")}
            <button onclick={() => run(() => api.revokeApproval(id))}>Revoke approval</button>
          {/if}
          {#if p.actions.includes("exportPackage")}
            <button onclick={() => run(() => api.exportPackage(id), "Exporting package…")}>Export package</button>
          {/if}
          {#if p.packageDir}
            <button onclick={() => run(() => api.openFolder(p.packageDir!))}>Open package folder</button>
          {/if}
          {#if p.actions.includes("cancel")}
            <button class="danger" onclick={() => run(() => api.cancelPublication(id))}>Cancel publication</button>
          {/if}
        </div>
      </div>
    </div>
  {:else}
    <p class="muted">Loading…</p>
  {/if}
</section>
