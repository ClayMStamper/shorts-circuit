<script lang="ts">
  import { api } from "../lib/api";
  import { formatDuration } from "../lib/format";
  import { fileUrl, pickFiles, VIDEO_EXTENSIONS } from "../lib/tauri";
  import type { AssetSummary, TagCount } from "../lib/types";
  import Stars from "./Stars.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    revision,
    onOpen,
    onImport,
  }: { revision: number; onOpen: (id: string) => void; onImport: (paths: string[]) => void } = $props();

  let text = $state("");
  let selectedTags = $state<string[]>([]);
  let minRating = $state(0);
  let awaitingOnly = $state(false);
  let assets = $state<AssetSummary[]>([]);
  let tags = $state<TagCount[]>([]);
  let thumbs = $state<Record<string, string>>({});
  let linkUrl = $state("");
  let linkTitle = $state("");
  let loading = $state(true);

  async function load() {
    try {
      const [list, counts] = await Promise.all([
        api.listAssets({
          text: text.trim() || undefined,
          tags: selectedTags,
          minRating: minRating || undefined,
          awaitingMedia: awaitingOnly ? true : undefined,
        }),
        api.tagCounts(),
      ]);
      assets = list;
      tags = counts;
      for (const a of list) {
        if (a.media && !thumbs[a.id]) {
          api
            .thumbnail(a.id)
            .then((path) => {
              if (path) thumbs[a.id] = fileUrl(path);
            })
            .catch(() => {});
        }
      }
    } catch (e) {
      errorToast(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    // Reload when filters change or the backend reports changes.
    void [text, selectedTags.length, minRating, awaitingOnly, revision];
    const timer = setTimeout(load, 150);
    return () => clearTimeout(timer);
  });

  function toggleTag(tag: string) {
    selectedTags = selectedTags.includes(tag)
      ? selectedTags.filter((t) => t !== tag)
      : [...selectedTags, tag];
  }

  async function chooseFiles() {
    const paths = await pickFiles({ multiple: true, extensions: VIDEO_EXTENSIONS, name: "Videos" });
    onImport(paths);
  }

  async function saveLink(event: SubmitEvent) {
    event.preventDefault();
    try {
      const id = await api.saveLink(linkUrl, linkTitle || null);
      linkUrl = "";
      linkTitle = "";
      toast("Link saved; attach the video when you have it.", "good");
      onOpen(id);
    } catch (e) {
      errorToast(e);
    }
  }
</script>

<section class="library">
  <div class="toolbar">
    <input class="search" type="search" placeholder="Search title, creator, notes, #tags…" bind:value={text} />
    <label>
      Min rating
      <select bind:value={minRating}>
        <option value={0}>Any</option>
        {#each [1, 2, 3, 4, 5] as n (n)}<option value={n}>{n}★+</option>{/each}
      </select>
    </label>
    <label class="check"><input type="checkbox" bind:checked={awaitingOnly} /> Awaiting media</label>
    <button class="primary" onclick={chooseFiles}>Import videos…</button>
  </div>

  {#if tags.length > 0}
    <div class="tag-filter">
      {#each tags as t (t.tag)}
        <button class="chip" class:active={selectedTags.includes(t.tag)} onclick={() => toggleTag(t.tag)}>
          #{t.tag} <small>{t.count}</small>
        </button>
      {/each}
    </div>
  {/if}

  <form class="link-form" onsubmit={saveLink}>
    <input type="url" placeholder="Save a link (e.g. a TikTok URL) to import its video later" bind:value={linkUrl} required />
    <input type="text" placeholder="Title (optional)" bind:value={linkTitle} />
    <button type="submit">Save link</button>
  </form>

  {#if !loading && assets.length === 0}
    <div class="empty">
      <p>No videos yet. Drag video files onto this window, use <strong>Import videos…</strong>, or set a watch
        folder in Settings.</p>
    </div>
  {/if}

  <ul class="grid">
    {#each assets as a (a.id)}
      <li>
        <button class="card" onclick={() => onOpen(a.id)}>
          <div class="thumb">
            {#if thumbs[a.id]}
              <img src={thumbs[a.id]} alt="" loading="lazy" />
            {:else}
              <span class="placeholder">{a.status === "awaiting_media" ? "Awaiting media" : "…"}</span>
            {/if}
            {#if a.media}<span class="duration">{formatDuration(a.media.properties.duration_ms)}</span>{/if}
          </div>
          <div class="meta">
            <strong>{a.title}</strong>
            {#if a.creator}<span class="muted">{a.creator}</span>{/if}
            <Stars value={a.rating} />
            <div class="tags">{#each a.tags as t (t)}<span>#{t}</span>{/each}</div>
            {#if a.publicationCount > 0}<span class="muted">{a.publicationCount} publication(s)</span>{/if}
          </div>
        </button>
      </li>
    {/each}
  </ul>
</section>
