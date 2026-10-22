<script lang="ts">
  import { onDestroy } from "svelte";
  import { api } from "../lib/api";
  import { actionLabel } from "../lib/format";
  import type { Action, JobView, MonitorView, PublicationView } from "../lib/types";
  import Badge from "./Badge.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    revision,
    onReview,
    onOpenAsset,
    onSettings = () => {},
  }: {
    revision: number;
    onReview: (id: string) => void;
    onOpenAsset: (id: string) => void;
    onSettings?: () => void;
  } = $props();

  let data = $state<MonitorView | null>(null);
  let postedUrl = $state<Record<string, string>>({});
  let resolving = $state<string | null>(null);
  let outcomeUrl = $state("");

  async function load() {
    try {
      data = await api.monitor();
    } catch (e) {
      errorToast(e);
    }
  }

  $effect(() => {
    void revision;
    load();
  });

  // Times in the monitor are relative to now; refresh periodically as a fallback for events.
  const timer = setInterval(load, 30_000);
  onDestroy(() => clearInterval(timer));

  async function run(action: () => Promise<unknown>, message?: string) {
    try {
      await action();
      if (message) toast(message, "good");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  function perform(p: PublicationView, action: Action) {
    switch (action) {
      case "edit":
      case "review":
      case "approve":
      case "reschedule":
        return onReview(p.id);
      case "submitForReview":
        return run(() => api.submitForReview(p.id));
      case "revokeApproval":
        return run(() => api.revokeApproval(p.id));
      case "publishNow":
        return run(() => api.schedule(p.id, { kind: "now" }), "Going out now.");
      case "cancel":
        return run(() => api.cancelPublication(p.id));
      case "retry":
        return run(() => api.retryPublication(p.id), "Retrying.");
      case "resolveUserAction":
        return run(() => api.resolveUserAction(p.id), "Continuing.");
      case "confirmPosted":
        return run(() => api.confirmPosted(p.id, postedUrl[p.id] ?? null), "Marked as posted.");
      case "resolveOutcome":
        resolving = p.id;
        return;
      case "exportPackage":
        return run(() => api.exportPackage(p.id), "Exporting package…");
      case "reconnect":
        toast(`Reconnect ${p.accountName} under YouTube Shorts publishing; the upload continues by itself.`, "info");
        return onSettings();
      case "sendToCloud":
        return run(() => api.sendToCloud(p.id), "The cloud runner has it now.");
      case "takeBack":
        return run(() => api.takeBackFromCloud(p.id), "Back on this computer.");
    }
  }

  function jobLabel(j: JobView): string {
    switch (j.kind) {
      case "render_variant":
        return "Render";
      case "publish_publication":
        return "Publish";
      case "export_package":
        return "Export";
    }
  }

  const ACTIVE = ["queued", "running", "paused"];
</script>

<section class="monitor">
  <h1>Publications</h1>
  {#if data && data.publications.length === 0}
    <p class="muted">Nothing scheduled. Create publications from an item in the library.</p>
  {/if}
  {#if data}
    <table>
      <thead>
        <tr><th>Video</th><th>Destination</th><th>Status</th><th>Posting time</th><th></th></tr>
      </thead>
      <tbody>
        {#each data.publications as p (p.id)}
          <tr>
            <td><button class="link" onclick={() => onOpenAsset(p.assetId)}>{p.assetTitle}</button></td>
            <td>{p.platformName}<br /><span class="muted">{p.accountName}</span></td>
            <td>
              <Badge state={p.state} />
              {#if p.owner === "cloud"}<span class="note" data-testid="cloud-owned">cloud</span>{/if}
              <div class="status-text">{p.statusText}</div>
              {#if p.cloudNote}<div class="note" data-testid="cloud-note">{p.cloudNote}</div>{/if}
              {#if p.progressFraction !== null && p.state === "uploading"}
                <progress max="1" value={p.progressFraction}></progress> {Math.round(p.progressFraction * 100)}%
              {/if}
              {#if p.progressText}<div class="muted" data-testid="progress">{p.progressText}</div>{/if}
              {#if p.publishedUrl && !p.publishedUrl.startsWith("file:")}
                <button class="link" onclick={() => run(() => api.openLink(p.publishedUrl!))}>Open on {p.platformName}</button>
              {/if}
              {#if p.publishedNote}<div class="note" data-testid="published-note">{p.publishedNote}</div>{/if}
            </td>
            <td>{p.scheduledLocal ?? "—"}</td>
            <td class="actions">
              {#each p.actions as a (a)}
                {#if a === "confirmPosted"}
                  <input placeholder="Post URL (optional)" bind:value={postedUrl[p.id]} />
                {/if}
                <button onclick={() => perform(p, a)}>{actionLabel(a)}</button>
              {/each}
              {#if p.packageDir}
                <button onclick={() => run(() => api.openFolder(p.packageDir!))}>Open package</button>
              {/if}
              {#if resolving === p.id}
                <div class="resolve">
                  <p>Check {p.platformName}. Was it published?</p>
                  <input placeholder="Post URL" bind:value={outcomeUrl} />
                  <button
                    onclick={() =>
                      run(() => api.resolveOutcome(p.id, { kind: "published", url: outcomeUrl || null, remote_id: null }), "Recorded as published.").then(() => (resolving = null))}
                    >Yes, it's live</button
                  >
                  <button
                    onclick={() =>
                      run(() => api.resolveOutcome(p.id, { kind: "not_published" }), "Publishing again.").then(() => (resolving = null))}
                    >No, publish again</button
                  >
                </div>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>

    <h2>Jobs</h2>
    <table class="jobs">
      <thead>
        <tr><th>Job</th><th>State</th><th>Run at (UTC)</th><th>Attempts</th><th>Last error</th><th></th></tr>
      </thead>
      <tbody>
        {#each data.jobs as j (j.id)}
          <tr>
            <td>{jobLabel(j)}</td>
            <td><span class="badge {j.state}">{j.state}{j.pauseReason ? ` (${j.pauseReason.replaceAll("_", " ")})` : ""}</span></td>
            <td>{j.runAt.replace("T", " ").slice(0, 16)}</td>
            <td>{j.claims}{j.failures ? ` (${j.failures} failed)` : ""}</td>
            <td class="muted">{j.lastError ?? ""}</td>
            <td class="actions">
              {#if j.kind !== "publish_publication" && (j.state === "failed" || j.state === "paused")}
                <button onclick={() => run(() => api.retryJob(j.id))}>Retry</button>
              {/if}
              {#if j.kind !== "publish_publication" && ACTIVE.includes(j.state)}
                <button onclick={() => run(() => api.cancelJob(j.id))}>Cancel</button>
              {/if}
              {#if j.packageDir}
                <button onclick={() => run(() => api.openFolder(j.packageDir!))}>Open package</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>
