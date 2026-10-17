<script lang="ts">
  import { api } from "../lib/api";
  import { formatBytes, formatDuration } from "../lib/format";
  import { defaultRecipe } from "../lib/recipe";
  import { fileUrl, pickFiles, VIDEO_EXTENSIONS } from "../lib/tauri";
  import type { AssetDetail, CollectionView, PermissionStatus } from "../lib/types";
  import Badge from "./Badge.svelte";
  import PublicationForm from "./PublicationForm.svelte";
  import Stars from "./Stars.svelte";
  import VariantEditor from "./VariantEditor.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    id,
    revision,
    onBack,
    onReview,
  }: {
    id: string;
    revision: number;
    onBack: () => void;
    onReview: (publicationId: string) => void;
  } = $props();

  let detail = $state<AssetDetail | null>(null);
  let title = $state("");
  let creator = $state("");
  let sourceUrl = $state("");
  let notes = $state("");
  let newTag = $state("");
  let videoNote = $state("");
  let audioNote = $state("");
  let openVariant = $state<string | null>(null);
  let creatingPublication = $state(false);
  let collections = $state<CollectionView[]>([]);
  let addCollection = $state("");

  async function load() {
    try {
      api
        .listCollections()
        .then((c) => (collections = c))
        .catch(() => {});
      const d = await api.assetDetail(id);
      const first = detail === null || detail.asset.id !== d.asset.id;
      detail = d;
      if (first) {
        title = d.asset.title;
        creator = d.asset.creator ?? "";
        sourceUrl = d.asset.sourceUrl ?? "";
        notes = d.asset.notes;
        videoNote = d.asset.videoPermission.note ?? "";
        audioNote = d.asset.audioPermission.note ?? "";
        openVariant = d.variants[0]?.id ?? null;
      }
    } catch (e) {
      errorToast(e);
    }
  }

  $effect(() => {
    void [id, revision];
    load();
  });

  async function save(update: Parameters<typeof api.updateAsset>[1]) {
    try {
      await api.updateAsset(id, update);
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  function saveText(event: SubmitEvent) {
    event.preventDefault();
    save({ title, creator, sourceUrl, notes });
    toast("Saved", "good");
  }

  function addTag(event: SubmitEvent) {
    event.preventDefault();
    if (!newTag.trim()) return;
    save({ addTags: newTag.split(/[\s,]+/).filter(Boolean) });
    newTag = "";
  }

  async function attachMedia() {
    const paths = await pickFiles({ multiple: false, extensions: VIDEO_EXTENSIONS, name: "Video" });
    if (paths.length === 0) return;
    try {
      const [result] = await api.importFiles(paths, id);
      if (result?.outcome === "failed") toast(result.message ?? "import failed", "bad");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  async function newVariant() {
    try {
      const v = await api.createVariant(id, `Variant ${(detail?.variants.length ?? 0) + 1}`, defaultRecipe());
      await load();
      openVariant = v.id;
    } catch (e) {
      errorToast(e);
    }
  }

  async function joinCollection(event: SubmitEvent) {
    event.preventDefault();
    if (!addCollection) return;
    try {
      const created = await api.addToCollection(addCollection, id);
      addCollection = "";
      if (created.length > 0) toast(`Rules created ${created.length} publication(s) for review`, "good");
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  async function leaveCollection(collectionId: string) {
    try {
      await api.removeFromCollection(collectionId, id);
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  const permissionOptions: PermissionStatus[] = ["unknown", "granted", "denied"];
</script>

<section class="asset-detail">
  <button class="link" onclick={onBack}>← Library</button>
  {#if detail}
    {@const a = detail.asset}
    <div class="columns">
      <div class="col">
        {#if a.media}
          <!-- svelte-ignore a11y_media_has_caption -->
          <video class="preview" src={fileUrl(a.media.path)} controls preload="metadata"></video>
          <p class="muted">
            {a.media.originalName} · {a.media.properties.width}x{a.media.properties.height} ·
            {formatDuration(a.media.properties.duration_ms)} · {formatBytes(a.media.sizeBytes)}
            {#if a.media.properties.video_codec}· {a.media.properties.video_codec}{/if}
            {#if a.media.properties.audio_codec}/{a.media.properties.audio_codec}{/if}
          </p>
        {:else}
          <div class="placeholder-box">
            {#if a.sourcePost}
              <p>
                Detected on TikTok (posted {new Date(a.sourcePost.postedAt).toLocaleString()}). Save the original file to
                the watch folder and confirm the match in the library, or attach it here.
              </p>
            {/if}
            <p>This item has no video yet.</p>
            <button class="primary" onclick={attachMedia}>Attach video…</button>
          </div>
        {/if}

        <form class="form" onsubmit={saveText}>
          <label>Title <input bind:value={title} required /></label>
          <label>Creator <input bind:value={creator} /></label>
          <label>Source URL <input type="url" bind:value={sourceUrl} /></label>
          <label>Notes <textarea rows="3" bind:value={notes}></textarea></label>
          <button type="submit">Save details</button>
        </form>

        <div class="field">
          <span class="label">Rating</span>
          <Stars value={a.rating} onChange={(stars) => save({ rating: stars })} />
        </div>

        <div class="field">
          <span class="label">Tags</span>
          <div class="tags editable">
            {#each a.tags as t (t)}
              <span class="chip">#{t} <button aria-label={`Remove ${t}`} onclick={() => save({ removeTags: [t] })}>×</button></span>
            {/each}
          </div>
          <form class="inline" onsubmit={addTag}>
            <input placeholder="Add tags" bind:value={newTag} />
            <button type="submit">Add</button>
          </form>
        </div>

        <div class="field">
          <span class="label">Collections</span>
          <div class="tags editable">
            {#each detail.collections as c (c.id)}
              <span class="chip">{c.name} <button aria-label={`Remove from ${c.name}`} onclick={() => leaveCollection(c.id)}>×</button></span>
            {/each}
          </div>
          {#if collections.some((c) => !detail?.collections.some((m) => m.id === c.id))}
            <form class="inline" onsubmit={joinCollection}>
              <select bind:value={addCollection} aria-label="Add to collection">
                <option value="">Add to collection…</option>
                {#each collections.filter((c) => !detail?.collections.some((m) => m.id === c.id)) as c (c.id)}
                  <option value={c.id}>{c.name}{c.rules.some((r) => r.enabled) ? " (has rules)" : ""}</option>
                {/each}
              </select>
              <button type="submit" disabled={!addCollection}>Add</button>
            </form>
          {/if}
        </div>

        <fieldset class="permissions">
          <legend>Permissions</legend>
          <label>
            Video
            <select value={a.videoPermission.status} onchange={(e) => save({ videoPermission: { status: e.currentTarget.value as PermissionStatus, note: videoNote } })}>
              {#each permissionOptions as o (o)}<option value={o}>{o}</option>{/each}
            </select>
            <input placeholder="Note (who, licence…)" bind:value={videoNote} onchange={() => save({ videoPermission: { status: a.videoPermission.status, note: videoNote } })} />
          </label>
          <label>
            Audio
            <select value={a.audioPermission.status} onchange={(e) => save({ audioPermission: { status: e.currentTarget.value as PermissionStatus, note: audioNote } })}>
              {#each permissionOptions as o (o)}<option value={o}>{o}</option>{/each}
            </select>
            <input placeholder="Note (sound source, licence…)" bind:value={audioNote} onchange={() => save({ audioPermission: { status: a.audioPermission.status, note: audioNote } })} />
          </label>
          <p class="muted">Audio is tracked separately: platform sounds are often licensed for that platform only.</p>
        </fieldset>
      </div>

      <div class="col">
        <h2>Variants</h2>
        {#each detail.variants as v (v.id)}
          <div class="panel">
            <button class="panel-head" onclick={() => (openVariant = openVariant === v.id ? null : v.id)}>
              <strong>{v.name}</strong>
              <span class="muted">rev {v.revision} · {v.rendered ? "rendered" : v.rendering ? "rendering…" : "not rendered"}</span>
            </button>
            {#if openVariant === v.id && a.media}
              <VariantEditor variant={v} source={a.media.properties} onSaved={load} />
            {/if}
          </div>
        {/each}
        {#if a.media}<button onclick={newVariant}>New variant</button>{/if}

        <h2>Publications</h2>
        {#if detail.publications.length === 0}<p class="muted">None yet.</p>{/if}
        <ul class="list">
          {#each detail.publications as p (p.id)}
            <li>
              <Badge state={p.state} />
              <strong>{p.platformName}</strong> · {p.accountName}
              <span class="muted">{p.scheduledLocal ?? "no time set"}</span>
              <button onclick={() => onReview(p.id)}>Open</button>
            </li>
          {/each}
        </ul>
        {#if a.media}
          {#if creatingPublication}
            <PublicationForm
              assetId={a.id}
              variants={detail.variants}
              onCreated={(pid) => {
                creatingPublication = false;
                onReview(pid);
              }}
              onCancel={() => (creatingPublication = false)}
            />
          {:else}
            <button class="primary" onclick={() => (creatingPublication = true)}>New publication</button>
          {/if}
        {/if}
      </div>
    </div>
  {:else}
    <p class="muted">Loading…</p>
  {/if}
</section>
