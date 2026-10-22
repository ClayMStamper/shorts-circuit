<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { timeAgo } from "../lib/format";
  import { pickFolder } from "../lib/tauri";
  import type { Preferences } from "../lib/types";
  import TikTokSettings from "./TikTokSettings.svelte";
  import Pill from "./ui/Pill.svelte";
  import PlatformIcon from "./ui/PlatformIcon.svelte";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision = 0 }: { revision?: number } = $props();

  let prefs = $state<Preferences | null>(null);
  let typed = $state("");

  async function load() {
    try {
      prefs = await api.preferences();
    } catch (e) {
      errorToast(e);
    }
  }

  onMount(load);
  $effect(() => {
    void revision;
    load();
  });

  async function setFolder(folder: string | null) {
    try {
      prefs = await api.updatePreferences({ watchFolder: folder });
      toast(folder ? "Watching folder" : "Stopped watching", "good");
    } catch (e) {
      errorToast(e);
    }
  }

  async function choose() {
    const dir = await pickFolder();
    if (dir) await setFolder(dir);
  }

  async function setDownloads(folder: string | null) {
    try {
      prefs = await api.updatePreferences({ downloadsFolder: folder });
      toast(folder ? "Picking up TikTok downloads" : "Stopped watching downloads", "good");
    } catch (e) {
      errorToast(e);
    }
  }

  async function toggleDownloads(on: boolean) {
    if (!on) return setDownloads(null);
    const dir = (await api.defaultDownloadsFolder().catch(() => null)) ?? (await pickFolder());
    if (dir) await setDownloads(dir);
  }

  async function chooseDownloads() {
    const dir = await pickFolder();
    if (dir) await setDownloads(dir);
  }

  async function checkNow() {
    try {
      prefs = await api.checkWatchFolder();
    } catch (e) {
      errorToast(e);
    }
  }
</script>

<article class="box conn-card" aria-labelledby="source-title" data-testid="source-card">
  <header>
    <PlatformIcon platform="tiktok" />
    <h3 id="source-title">Source: your TikTok videos</h3>
    <Pill tone={prefs?.watchFolder ? "good" : "warn"} label={prefs?.watchFolder ? "Watching" : "No folder yet"} />
  </header>
  <p class="muted">
    TikTok's API can't hand over your video files, so originals come from a folder: the one your phone or editor saves
    finished videos to (OneDrive, Google Drive, Dropbox, iCloud, or the editor's export folder). A caption file next to a
    video (same name, .txt or .json) is used as its caption.
  </p>
  <div class="setting">
    <span class="label">Watch folder</span>
    {#if prefs?.watchFolder}
      <p><code>{prefs.watchFolder}</code></p>
      <p class="muted">
        {prefs.watchCheckedAt ? `Last checked ${timeAgo(prefs.watchCheckedAt)}` : "Not checked yet"} ·
        <button class="link" onclick={checkNow}>Check now</button>
      </p>
      <div class="btn-row">
        <button onclick={choose}>Change…</button>
        <button class="ghost" onclick={() => setFolder(null)}>Stop watching</button>
      </div>
    {:else}
      <div class="btn-row">
        <button class="primary" onclick={choose}>Choose folder…</button>
      </div>
      <form
        class="btn-row"
        aria-label="Watch folder path"
        onsubmit={(e) => {
          e.preventDefault();
          if (typed.trim()) setFolder(typed.trim());
        }}
      >
        <label class="sr-only" for="watch-path">Folder path</label>
        <input id="watch-path" placeholder="or paste a folder path" bind:value={typed} style="flex: 1" />
        <button type="submit">Use this folder</button>
      </form>
    {/if}
  </div>
  <div class="setting" data-testid="downloads-setting">
    <label class="check">
      <input
        type="checkbox"
        checked={!!prefs?.downloadsFolder}
        onchange={(e) => toggleDownloads(e.currentTarget.checked)}
      />
      Also pick up originals I download from TikTok
    </label>
    <p class="muted">
      On a video that's waiting for its original, click <strong>Get original ↗</strong>, then right-click the video on
      TikTok → <strong>Download video</strong>. The app takes it from your downloads folder and publishes it with the
      post's caption. Other files there are left alone.
    </p>
    {#if prefs?.downloadsFolder}
      <p><code>{prefs.downloadsFolder}</code> <button class="link" onclick={chooseDownloads}>Change…</button></p>
    {/if}
  </div>
  <details>
    <summary>TikTok captions (optional): connect TikTok so each video uses its post's caption and hashtags</summary>
    <TikTokSettings {revision} />
  </details>
</article>
