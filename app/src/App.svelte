<script lang="ts">
  import { onMount, tick } from "svelte";
  import { api } from "./lib/api";
  import { onAppEvent, onFileDrop } from "./lib/tauri";
  import type { AppEvent, MediaStatus } from "./lib/types";
  import Library from "./components/Library.svelte";
  import AssetDetail from "./components/AssetDetail.svelte";
  import Review from "./components/Review.svelte";
  import Monitor from "./components/Monitor.svelte";
  import Settings from "./components/Settings.svelte";
  import Collections from "./components/Collections.svelte";
  import Workflows from "./components/Workflows.svelte";
  import WorkflowDetail from "./components/WorkflowDetail.svelte";
  import Activity from "./components/Activity.svelte";
  import Connections from "./components/Connections.svelte";
  import Toasts, { toast } from "./components/Toasts.svelte";

  type Route =
    | { view: "workflows" }
    | { view: "workflow"; id: string; settings?: boolean }
    | { view: "activity" }
    | { view: "connections" }
    | { view: "settings" }
    // Advanced (under Settings):
    | { view: "library" }
    | { view: "asset"; id: string }
    | { view: "review"; id: string }
    | { view: "monitor" }
    | { view: "collections" };

  type Section = "workflows" | "activity" | "connections" | "settings";

  const NAV: Array<{ section: Section; label: string }> = [
    { section: "workflows", label: "Workflows" },
    { section: "activity", label: "Activity" },
    { section: "connections", label: "Connections" },
    { section: "settings", label: "Settings" },
  ];

  let route = $state<Route>({ view: "workflows" });
  const section = $derived<Section>(
    route.view === "workflow"
      ? "workflows"
      : route.view === "activity" || route.view === "connections" || route.view === "workflows"
        ? route.view
        : "settings",
  );
  const advanced = $derived(["library", "asset", "review", "monitor", "collections"].includes(route.view));
  const ADVANCED_LABEL: Record<string, string> = {
    library: "Library",
    asset: "Library",
    review: "Review",
    monitor: "Monitor",
    collections: "Collections",
  };
  let dragging = $state(false);
  let media = $state<MediaStatus | null>(null);
  // Bumped on backend events so screens reload their data.
  let revision = $state(0);

  function go(next: Route) {
    route = next;
    // Screen readers and keyboard users start at the top of the new page.
    tick().then(() => document.getElementById("main")?.focus({ preventScroll: true }));
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
        toast(`A workflow couldn't handle a video: ${event.message}`, "bad");
        break;
      case "originalAttached":
        toast(`Got the original for “${event.title}”. Its workflows take it from here.`, "good");
        break;
      case "matchSuggested":
        toast("A new file may be the original of a TikTok post; confirm it in Settings → Advanced → Library.", "info");
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
  <a class="skip-link" href="#main">Skip to content</a>
  <header class="topbar">
    <div class="brand">Shorts Circuit</div>
    <nav aria-label="Main">
      {#each NAV as item (item.section)}
        <button
          class:active={section === item.section}
          aria-current={section === item.section ? "page" : undefined}
          onclick={() => go({ view: item.section })}
        >
          {item.label}
        </button>
      {/each}
    </nav>
  </header>

  {#if media && !media.available}
    <div class="banner warn" role="alert">
      FFmpeg was not found, so videos can't be inspected or rendered. {media.message}
    </div>
  {/if}

  <main id="main" tabindex="-1">
    {#if advanced}
      <nav class="crumbs" aria-label="Breadcrumb">
        <button class="link" onclick={() => go({ view: "settings" })}>Settings</button><span aria-hidden="true">›</span>
        <span>Advanced</span><span aria-hidden="true">›</span>
        <span aria-current="page">{ADVANCED_LABEL[route.view]}</span>
      </nav>
    {/if}
    {#if route.view === "workflows"}
      <Workflows
        {revision}
        onOpen={(id, settings) => go({ view: "workflow", id, settings })}
        onConnections={() => go({ view: "connections" })}
      />
    {:else if route.view === "workflow"}
      <WorkflowDetail
        id={route.id}
        openSettings={route.settings}
        {revision}
        onBack={() => go({ view: "workflows" })}
        onConnections={() => go({ view: "connections" })}
        onReview={(id) => go({ view: "review", id })}
      />
    {:else if route.view === "activity"}
      <Activity {revision} onConnections={() => go({ view: "connections" })} onReview={(id) => go({ view: "review", id })} />
    {:else if route.view === "connections"}
      <Connections {revision} />
    {:else if route.view === "library"}
      <Library {revision} onOpen={(id) => go({ view: "asset", id })} onImport={importPaths} />
    {:else if route.view === "asset"}
      <AssetDetail
        id={route.id}
        {revision}
        onBack={() => go({ view: "library" })}
        onReview={(id) => go({ view: "review", id })}
      />
    {:else if route.view === "review"}
      <Review id={route.id} {revision} onBack={() => go({ view: "activity" })} />
    {:else if route.view === "monitor"}
      <Monitor
        {revision}
        onReview={(id) => go({ view: "review", id })}
        onOpenAsset={(id) => go({ view: "asset", id })}
        onSettings={() => go({ view: "connections" })}
      />
    {:else if route.view === "collections"}
      <Collections {revision} onReview={(id) => go({ view: "review", id })} />
    {:else}
      <Settings {revision} onAdvanced={(to) => go({ view: to })} />
    {/if}
  </main>

  {#if dragging}
    <div class="drop-overlay">Drop videos to import them</div>
  {/if}
  <Toasts />
</div>
