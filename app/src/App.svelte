<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "./lib/api";
  import { onAppEvent, onFileDrop } from "./lib/tauri";
  import type { AppEvent, MediaStatus } from "./lib/types";
  import Library from "./components/Library.svelte";
  import AssetDetail from "./components/AssetDetail.svelte";
  import Review from "./components/Review.svelte";
  import Monitor from "./components/Monitor.svelte";
  import Settings from "./components/Settings.svelte";
  import Collections from "./components/Collections.svelte";
  import Toasts, { toast } from "./components/Toasts.svelte";

  type Route =
    | { view: "library" }
    | { view: "asset"; id: string }
    | { view: "review"; id: string }
    | { view: "monitor" }
    | { view: "collections" }
    | { view: "settings" };

  let route = $state<Route>({ view: "library" });
  let dragging = $state(false);
  let media = $state<MediaStatus | null>(null);
  // Bumped on backend events so screens reload their data.
  let revision = $state(0);

  function go(next: Route) {
    route = next;
  }

  async function importPaths(paths: string[]) {
    if (paths.length === 0) return;
    try {
      const results = await api.importFiles(paths);
      for (const r of results) {
        const name = r.path.split(/[\\/]/).pop() ?? r.path;
        if (r.outcome === "imported") toast(`Imported ${name}`, "good");
        else if (r.outcome === "duplicate") toast(`${name} is already in the library`, "info");
        else toast(`${name}: ${r.message ?? "import failed"}`, "bad");
      }
    } catch (e) {
      toast((e as { message: string }).message, "bad");
    }
  }

  function handleEvent(event: AppEvent) {
    switch (event.type) {
      case "importFailed":
        toast(`Watch folder: ${event.path.split(/[\\/]/).pop()}: ${event.message}`, "bad");
        break;
      case "watchFolderError":
        toast(`Watch folder: ${event.message}`, "bad");
        break;
      case "rulesFailed":
        toast(`Collection rules: ${event.message}`, "bad");
        break;
      case "matchSuggested":
        toast("A new file may be the original of a TikTok post; check the library.", "info");
        break;
      case "renderProgress":
      case "watchFolderChecked":
        return;
    }
    revision += 1;
  }

  onMount(() => {
    const unlisten: Array<() => void> = [];
    onAppEvent(handleEvent).then((u) => unlisten.push(u));
    onFileDrop({ over: (h) => (dragging = h), drop: importPaths }).then((u) => unlisten.push(u));
    api
      .mediaStatus()
      .then((s) => (media = s))
      .catch(() => {});
    return () => unlisten.forEach((u) => u());
  });
</script>

<div class="shell">
  <header class="topbar">
    <div class="brand">Shorts Circuit</div>
    <nav>
      <button class:active={route.view === "library" || route.view === "asset"} onclick={() => go({ view: "library" })}>
        Library
      </button>
      <button class:active={route.view === "monitor" || route.view === "review"} onclick={() => go({ view: "monitor" })}>
        Monitor
      </button>
      <button class:active={route.view === "collections"} onclick={() => go({ view: "collections" })}>
        Collections
      </button>
      <button class:active={route.view === "settings"} onclick={() => go({ view: "settings" })}>Settings</button>
    </nav>
  </header>

  {#if media && !media.available}
    <div class="banner warn">
      FFmpeg was not found, so videos can't be inspected or rendered. {media.message}
    </div>
  {/if}

  <main>
    {#if route.view === "library"}
      <Library {revision} onOpen={(id) => go({ view: "asset", id })} onImport={importPaths} />
    {:else if route.view === "asset"}
      <AssetDetail
        id={route.id}
        {revision}
        onBack={() => go({ view: "library" })}
        onReview={(id) => go({ view: "review", id })}
      />
    {:else if route.view === "review"}
      <Review id={route.id} {revision} onBack={() => go({ view: "monitor" })} />
    {:else if route.view === "monitor"}
      <Monitor
        {revision}
        onReview={(id) => go({ view: "review", id })}
        onOpenAsset={(id) => go({ view: "asset", id })}
        onSettings={() => go({ view: "settings" })}
      />
    {:else if route.view === "collections"}
      <Collections {revision} onReview={(id) => go({ view: "review", id })} />
    {:else}
      <Settings {revision} />
    {/if}
  </main>

  {#if dragging}
    <div class="drop-overlay">Drop videos to import them</div>
  {/if}
  <Toasts />
</div>
