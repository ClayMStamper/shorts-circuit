<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { formatBytes, formatDuration, parseTimecode } from "../lib/format";
  import { centreCrop, cloneRecipe, defaultRecipe, outputDuration, recipeIssues, X264_PRESETS } from "../lib/recipe";
  import { fileUrl, onAppEvent, pickFiles } from "../lib/tauri";
  import type { MediaProperties, RenderRecipe, VariantView } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let {
    variant,
    source,
    onSaved,
  }: { variant: VariantView; source: MediaProperties; onSaved: () => void } = $props();

  let name = $state("");
  let recipe = $state<RenderRecipe>(defaultRecipe());
  let trimStart = $state("");
  let trimEnd = $state("");
  let progress = $state<number | null>(null);
  let loadedRevision = $state(-1);

  $effect(() => {
    // Reset the form when a new revision of the variant arrives.
    if (variant.revision !== loadedRevision) {
      loadedRevision = variant.revision;
      name = variant.name;
      recipe = cloneRecipe(variant.recipe);
      trimStart = recipe.trim ? formatDuration(recipe.trim.start_ms) : "";
      trimEnd = recipe.trim ? formatDuration(recipe.trim.end_ms) : "";
    }
  });

  let trimError = $derived.by(() => {
    if (!trimStart && !trimEnd) return null;
    const s = parseTimecode(trimStart || "0");
    const e = parseTimecode(trimEnd || formatDuration(source.duration_ms));
    return s === null || e === null ? "Use m:ss.s, e.g. 0:12.5" : null;
  });

  function applyTrim() {
    if (!trimStart && !trimEnd) {
      recipe.trim = null;
      return;
    }
    const start = parseTimecode(trimStart || "0");
    const end = parseTimecode(trimEnd || formatDuration(source.duration_ms));
    if (start !== null && end !== null) recipe.trim = { start_ms: start, end_ms: end };
  }

  let issues = $derived(recipeIssues(recipe, source));
  let dirty = $derived(name !== variant.name || JSON.stringify(recipe) !== JSON.stringify(variant.recipe));

  async function save() {
    applyTrim();
    try {
      await api.updateVariant(variant.id, name, $state.snapshot(recipe));
      toast(
        "Variant saved. Publications using it get a new revision and need approval again.",
        "info",
      );
      onSaved();
    } catch (e) {
      errorToast(e);
    }
  }

  async function render() {
    try {
      progress = 0;
      await api.renderVariant(variant.id);
      onSaved();
    } catch (e) {
      progress = null;
      errorToast(e);
    }
  }

  async function chooseSubtitles() {
    const [path] = await pickFiles({ multiple: false, extensions: ["srt", "ass", "vtt"], name: "Subtitles" });
    if (path) recipe.subtitles = { path, mode: recipe.subtitles?.mode ?? "burn_in", language: null };
  }

  async function chooseAudio() {
    const [path] = await pickFiles({ multiple: false, extensions: ["m4a", "mp3", "wav", "aac", "flac", "ogg"], name: "Audio" });
    if (path) recipe.audio = { kind: "replace", path };
  }

  function setAudioKind(kind: string) {
    if (kind === "mute") recipe.audio = { kind: "mute" };
    else if (kind === "original") recipe.audio = { kind: "original", track: 0 };
    else recipe.audio = { kind: "replace", path: recipe.audio.kind === "replace" ? recipe.audio.path : "" };
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    onAppEvent((e) => {
      if (e.type === "renderProgress" && e.variantId === variant.id) {
        progress = e.fraction >= 1 ? null : e.fraction;
      }
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  });
</script>

<div class="variant-editor">
  <label>Name <input bind:value={name} /></label>

  <fieldset>
    <legend>Trim</legend>
    <label>Start <input placeholder="0:00.0" bind:value={trimStart} onchange={applyTrim} /></label>
    <label>End <input placeholder={formatDuration(source.duration_ms)} bind:value={trimEnd} onchange={applyTrim} /></label>
    {#if trimError}<span class="error">{trimError}</span>{/if}
    <span class="muted">Output {formatDuration(outputDuration(recipe, source.duration_ms))}</span>
  </fieldset>

  <fieldset>
    <legend>Frame</legend>
    <label>
      Fit
      <select bind:value={recipe.fit}>
        <option value="pad">Fit inside (black bars)</option>
        <option value="fill">Fill frame (crop overflow)</option>
      </select>
    </label>
    {#if recipe.crop}
      <div class="row">
        <label>x <input type="number" min="0" bind:value={recipe.crop.x} /></label>
        <label>y <input type="number" min="0" bind:value={recipe.crop.y} /></label>
        <label>w <input type="number" min="2" step="2" bind:value={recipe.crop.width} /></label>
        <label>h <input type="number" min="2" step="2" bind:value={recipe.crop.height} /></label>
        <button type="button" onclick={() => (recipe.crop = null)}>Remove crop</button>
      </div>
    {:else}
      <button type="button" onclick={() => (recipe.crop = centreCrop(source, recipe))}>Crop centre to output aspect</button>
    {/if}
  </fieldset>

  <fieldset>
    <legend>Subtitles</legend>
    {#if recipe.subtitles}
      <span class="path">{recipe.subtitles.path}</span>
      <select bind:value={recipe.subtitles.mode}>
        <option value="burn_in">Burn into picture</option>
        <option value="soft">Selectable track</option>
      </select>
      <button type="button" onclick={() => (recipe.subtitles = null)}>Remove</button>
    {:else}
      <button type="button" onclick={chooseSubtitles}>Add subtitle file…</button>
    {/if}
  </fieldset>

  <fieldset>
    <legend>Audio</legend>
    <select value={recipe.audio.kind} onchange={(e) => setAudioKind(e.currentTarget.value)}>
      <option value="original">Keep original</option>
      <option value="mute">Mute</option>
      <option value="replace">Replace with file</option>
    </select>
    {#if recipe.audio.kind === "original" && source.audio_streams > 1}
      <label>Track <input type="number" min="0" max={source.audio_streams - 1} bind:value={recipe.audio.track} /></label>
    {/if}
    {#if recipe.audio.kind === "replace"}
      <span class="path">{recipe.audio.path || "no file chosen"}</span>
      <button type="button" onclick={chooseAudio}>Choose…</button>
    {/if}
  </fieldset>

  <details>
    <summary>Encoding</summary>
    <div class="row">
      <label>Width <input type="number" step="2" bind:value={recipe.encoding.width} /></label>
      <label>Height <input type="number" step="2" bind:value={recipe.encoding.height} /></label>
      <label>FPS <input type="number" bind:value={recipe.encoding.fps} /></label>
      <label>CRF <input type="number" min="0" max="51" bind:value={recipe.encoding.crf} /></label>
      <label>
        Preset
        <select bind:value={recipe.encoding.preset}>
          {#each X264_PRESETS as p (p)}<option value={p}>{p}</option>{/each}
        </select>
      </label>
      <label>Audio kbps <input type="number" bind:value={recipe.encoding.audio_bitrate_kbps} /></label>
      <label>
        Sample rate
        <select bind:value={recipe.encoding.audio_sample_rate}>
          <option value={48000}>48000</option>
          <option value={44100}>44100</option>
        </select>
      </label>
    </div>
    <p class="muted">H.264 (x264) video, AAC audio, MP4 with fast start.</p>
  </details>

  {#if issues.length > 0}
    <ul class="issues">{#each issues as i (i)}<li>{i}</li>{/each}</ul>
  {/if}

  <div class="actions">
    <button class="primary" disabled={!dirty || issues.length > 0} onclick={save}>Save variant</button>
    <button disabled={dirty || variant.rendering || progress !== null || !!variant.rendered} onclick={render}>
      {variant.rendered ? "Rendered" : "Render"}
    </button>
    {#if progress !== null}<progress max="1" value={progress}></progress>{/if}
  </div>

  {#if variant.rendered}
    <!-- svelte-ignore a11y_media_has_caption -->
    <video class="preview vertical" src={fileUrl(variant.rendered.path)} controls preload="metadata"></video>
    <p class="muted">
      {variant.rendered.properties.width}x{variant.rendered.properties.height} ·
      {formatDuration(variant.rendered.properties.duration_ms)} · {formatBytes(variant.rendered.sizeBytes)}
    </p>
  {/if}
</div>
