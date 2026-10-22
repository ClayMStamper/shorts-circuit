<script lang="ts">
  import { untrack } from "svelte";
  import { api } from "../lib/api";
  import { parseHashtags, systemTimeZone, toLocalInput } from "../lib/format";
  import type { PostingMode, WorkflowUpdate, WorkflowView } from "../lib/types";
  import Drawer from "./ui/Drawer.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    workflow,
    onclose,
    onSaved,
  }: { workflow: WorkflowView; onclose: () => void; onSaved: (w: WorkflowView) => void } = $props();

  // Edited copies of the settings when the drawer opened; saved together.
  const initial = untrack(() => workflow);
  const tz = systemTimeZone();
  let name = $state(initial.name);
  let requireReview = $state(!initial.autoPublish);
  let posting = $state<PostingMode>(initial.posting);
  let template = $state(initial.captionTemplate);
  let hashtags = $state(initial.hashtags.map((t) => `#${t}`).join(" "));
  let shareToFeed = $state(initial.shareToFeed ?? true);
  let youtubePublic = $state(!initial.privateOnly);
  let rights = $state(initial.rightsConfirmed);
  let from = $state(toLocalInput(initial.effectiveFrom, tz));
  let busy = $state(false);

  async function save(e: SubmitEvent) {
    e.preventDefault();
    const update: WorkflowUpdate = {};
    if (name.trim() !== workflow.name) update.name = name.trim();
    if (requireReview === workflow.autoPublish) update.autoPublish = !requireReview;
    if (posting !== workflow.posting) update.posting = posting;
    if (template !== workflow.captionTemplate) update.captionTemplate = template;
    const tags = parseHashtags(hashtags);
    if (tags.join(" ") !== workflow.hashtags.join(" ")) update.hashtags = tags;
    if (workflow.platform === "instagram_reels" && shareToFeed !== (workflow.shareToFeed ?? true))
      update.shareToFeed = shareToFeed;
    if (rights !== workflow.rightsConfirmed) update.rightsConfirmed = rights;
    if (from !== toLocalInput(workflow.effectiveFrom, tz)) update.effectiveFrom = new Date(from).toISOString();
    busy = true;
    try {
      if (workflow.platform === "youtube_shorts" && youtubePublic === workflow.privateOnly) {
        if (
          youtubePublic &&
          !confirm(
            "Upload to this channel as public?\n\nOnly do this once your Google Cloud project has passed YouTube's API audit (or YouTube has shown it publishes your uploads publicly).",
          )
        ) {
          youtubePublic = false;
        } else {
          await api.youtubeSetPublicUploads(workflow.accountId, youtubePublic);
        }
      }
      const saved = await api.updateWorkflow(workflow.id, update);
      toast("Settings saved", "good");
      onSaved(saved);
    } catch (err) {
      errorToast(err);
    } finally {
      busy = false;
    }
  }
</script>

<Drawer title="Workflow settings" {onclose}>
  <form class="form" id="workflow-settings" onsubmit={save} aria-label="Workflow settings">
    <div class="setting">
      <label class="label" for="wf-name">Name</label>
      <input id="wf-name" bind:value={name} required />
    </div>

    <div class="setting">
      <label class="check"><input type="checkbox" bind:checked={requireReview} /> Require review</label>
      <p class="help">Each video waits for you to approve it (the same as turning Auto Publish off).</p>
    </div>

    <fieldset class="setting" style="border: none; padding: 0">
      <legend class="label">When to post</legend>
      <label class="check"><input type="radio" name="posting" value="asap" bind:group={posting} /> As soon as possible</label>
      <label class="check"><input type="radio" name="posting" value="slots" bind:group={posting} /> In my posting slots</label>
      <p class="help">Posting slots are set in Settings. With no free slot, it posts as soon as possible.</p>
    </fieldset>

    <div class="setting">
      <label class="label" for="wf-caption">Caption</label>
      <textarea id="wf-caption" rows="3" bind:value={template} placeholder={"{caption}"} aria-describedby="wf-caption-help"></textarea>
      <p class="help" id="wf-caption-help">
        <code>{"{caption}"}</code> is the source caption with its hashtags (the default). Also <code>{"{title}"}</code>,
        <code>{"{creator}"}</code>. Each platform's limits are applied automatically.
      </p>
    </div>

    <div class="setting">
      <label class="label" for="wf-tags">Extra hashtags</label>
      <input id="wf-tags" bind:value={hashtags} placeholder="#shorts #recipes" aria-describedby="wf-tags-help" />
      <p class="help" id="wf-tags-help">Added after the source's own hashtags.</p>
    </div>

    {#if workflow.platform === "youtube_shorts"}
      <div class="setting">
        <label class="check"><input type="checkbox" bind:checked={youtubePublic} /> Upload as public</label>
        <p class="help">Applies to the whole channel. Leave it off until your Google Cloud project passes YouTube's API audit; until then YouTube keeps uploads private.</p>
      </div>
    {:else if workflow.platform === "instagram_reels"}
      <div class="setting">
        <label class="check"><input type="checkbox" bind:checked={shareToFeed} /> Also share to the main feed</label>
      </div>
    {/if}

    <div class="setting">
      <label class="check"><input type="checkbox" bind:checked={rights} /> I own or have the rights to content from this source</label>
      <p class="help">Records video and audio permission for the videos this workflow handles.</p>
    </div>

    <div class="setting">
      <label class="label" for="wf-from">Include content from</label>
      <input id="wf-from" type="datetime-local" bind:value={from} required aria-describedby="wf-from-help" />
      <p class="help" id="wf-from-help">
        Videos that arrived (and were posted on TikTok) before this are skipped. An earlier date publishes your back catalog.
      </p>
    </div>
  </form>
  {#snippet footer()}
    <button type="button" onclick={onclose}>Cancel</button>
    <button class="primary" type="submit" form="workflow-settings" disabled={busy}>Save</button>
  {/snippet}
</Drawer>
