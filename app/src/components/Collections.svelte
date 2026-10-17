<script lang="ts">
  import { api } from "../lib/api";
  import type { AccountView, CollectionView, RuleInput, RuleOutput, RuleView } from "../lib/types";
  import { errorToast, toast } from "./Toasts.svelte";

  let { revision }: { revision: number } = $props();

  let collections = $state<CollectionView[]>([]);
  let accounts = $state<AccountView[]>([]);
  let selectedId = $state<string | null>(null);
  let newName = $state("");
  let editingRule = $state<string | null>(null);
  let showRuleForm = $state(false);
  let form = $state<RuleInput>(blankRule());
  let hashtagText = $state("");

  const selected = $derived(collections.find((c) => c.id === selectedId) ?? null);

  function blankRule(): RuleInput {
    return {
      name: "Cross-post",
      enabled: true,
      destinations: [],
      output: "awaiting_approval",
      captionTemplate: "{title}",
      hashtags: [],
      includeAssetTags: true,
    };
  }

  async function load() {
    try {
      [collections, accounts] = await Promise.all([api.listCollections(), api.listAccounts()]);
      if (!collections.some((c) => c.id === selectedId)) selectedId = collections[0]?.id ?? null;
    } catch (e) {
      errorToast(e);
    }
  }

  $effect(() => {
    void revision;
    load();
  });

  function replace(view: CollectionView) {
    collections = collections.map((c) => (c.id === view.id ? view : c));
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    try {
      const view = await api.createCollection(newName);
      newName = "";
      collections = [...collections, view].sort((a, b) => a.name.localeCompare(b.name));
      selectedId = view.id;
    } catch (e) {
      errorToast(e);
    }
  }

  async function rename(c: CollectionView, name: string) {
    if (name.trim() === c.name) return;
    try {
      replace(await api.renameCollection(c.id, name));
    } catch (e) {
      errorToast(e);
    }
  }

  async function remove(c: CollectionView) {
    if (!confirm(`Delete "${c.name}" and its rules? Its videos and publications are kept.`)) return;
    try {
      await api.deleteCollection(c.id);
      await load();
    } catch (e) {
      errorToast(e);
    }
  }

  function startRule(rule?: RuleView) {
    editingRule = rule?.id ?? null;
    form = rule
      ? {
          name: rule.name,
          enabled: rule.enabled,
          destinations: rule.destinations.map((d) => d.accountId),
          output: rule.output,
          captionTemplate: rule.captionTemplate,
          hashtags: [...rule.hashtags],
          includeAssetTags: rule.includeAssetTags,
        }
      : blankRule();
    hashtagText = form.hashtags.join(" ");
    showRuleForm = true;
  }

  async function saveRule(event: SubmitEvent) {
    event.preventDefault();
    if (!selected) return;
    const rule: RuleInput = { ...form, hashtags: hashtagText.split(/[\s,]+/).filter(Boolean) };
    try {
      replace(editingRule ? await api.updateRule(editingRule, rule) : await api.createRule(selected.id, rule));
      showRuleForm = false;
      toast("Rule saved", "good");
    } catch (e) {
      errorToast(e);
    }
  }

  async function toggleRule(rule: RuleView) {
    try {
      replace(
        await api.updateRule(rule.id, {
          name: rule.name,
          enabled: !rule.enabled,
          destinations: rule.destinations.map((d) => d.accountId),
          output: rule.output,
          captionTemplate: rule.captionTemplate,
          hashtags: rule.hashtags,
          includeAssetTags: rule.includeAssetTags,
        }),
      );
    } catch (e) {
      errorToast(e);
    }
  }

  async function deleteRule(rule: RuleView) {
    try {
      replace(await api.deleteRule(rule.id));
    } catch (e) {
      errorToast(e);
    }
  }

  async function setUnattended(c: CollectionView, accountId: string, enabled: boolean, name: string) {
    if (
      enabled &&
      !confirm(
        `Turn on unattended publishing for ${name} in "${c.name}"?\n\nPublications the rules create for this account ` +
          "will be approved and posted at their slot without asking you.",
      )
    ) {
      await load();
      return;
    }
    try {
      replace(await api.setUnattended(c.id, accountId, enabled));
    } catch (e) {
      errorToast(e);
      await load();
    }
  }

  const outputLabel: Record<RuleOutput, string> = {
    draft: "as drafts",
    awaiting_approval: "submitted for your approval",
  };
</script>

<section class="collections">
  <h1>Collections</h1>
  <div class="columns">
    <div class="col">
      <ul class="list" aria-label="Collections">
        {#each collections as c (c.id)}
          <li>
            <button class:primary={c.id === selectedId} onclick={() => (selectedId = c.id)}>{c.name}</button>
            <span class="muted">
              {c.memberCount} video{c.memberCount === 1 ? "" : "s"} · {c.rules.length} rule{c.rules.length === 1 ? "" : "s"}
              {#if c.watchFolderTarget}· watch folder{/if}
            </span>
          </li>
        {/each}
      </ul>
      {#if collections.length === 0}<p class="muted">No collections yet.</p>{/if}
      <form class="inline" onsubmit={create}>
        <input placeholder="New collection" bind:value={newName} required />
        <button type="submit">Create</button>
      </form>
      <p class="muted">
        Add videos to a collection from their detail page, or route watch-folder imports into one in Settings.
      </p>
    </div>

    <div class="col">
      {#if selected}
        {@const c = selected}
        <div class="row">
          <input
            aria-label="Collection name"
            value={c.name}
            onchange={(e) => rename(c, e.currentTarget.value)}
          />
          <button class="danger" onclick={() => remove(c)}>Delete collection</button>
        </div>
        {#if c.watchFolderTarget}
          <p class="muted">New watch-folder imports are added here.</p>
        {/if}

        <h2>Rules</h2>
        <p class="muted">
          When a video enters this collection (including through the watch folder), each enabled rule creates one
          publication per destination in that account's next free posting slot. A rule fires once per video and
          destination; re-imports and duplicates don't create more.
        </p>
        {#if c.rules.length === 0}<p class="muted">No rules yet.</p>{/if}
        <ul class="list">
          {#each c.rules as r (r.id)}
            <li class="panel" data-testid="rule">
              <strong>{r.name}</strong>
              {#if !r.enabled}<span class="badge neutral">off</span>{/if}
              <span>
                → {r.destinations.map((d) => `${d.platformName} · ${d.accountName}`).join(", ")},
                {outputLabel[r.output]}
              </span>
              {#if r.destinations.some((d) => d.unattended)}
                <span class="badge warn">unattended: {r.destinations.filter((d) => d.unattended).map((d) => d.accountName).join(", ")}</span>
              {/if}
              <span class="muted">{r.firedCount} created</span>
              <button onclick={() => startRule(r)}>Edit</button>
              <button onclick={() => toggleRule(r)}>{r.enabled ? "Turn off" : "Turn on"}</button>
              <button onclick={() => deleteRule(r)}>Delete</button>
            </li>
          {/each}
        </ul>

        {#if showRuleForm}
          <form class="form panel" onsubmit={saveRule} aria-label="Rule">
            <label>Name <input bind:value={form.name} required /></label>
            <fieldset>
              <legend>Create publications for</legend>
              {#each accounts as a (a.id)}
                <label class="check">
                  <input type="checkbox" value={a.id} bind:group={form.destinations} />
                  {a.platformName} · {a.displayName}
                </label>
              {/each}
              {#if accounts.length === 0}<p class="muted">Add destination accounts in Settings first.</p>{/if}
            </fieldset>
            <fieldset>
              <legend>Leave them</legend>
              <label class="check"><input type="radio" value="awaiting_approval" bind:group={form.output} /> Submitted for my approval</label>
              <label class="check"><input type="radio" value="draft" bind:group={form.output} /> As drafts I finish myself</label>
            </fieldset>
            <label>
              Caption
              <input bind:value={form.captionTemplate} />
            </label>
            <p class="muted">Use {"{title}"}, {"{creator}"} and {"{notes}"} for the video's details.</p>
            <label>Hashtags <input bind:value={hashtagText} placeholder="#shorts #recipe" /></label>
            <label class="check"><input type="checkbox" bind:checked={form.includeAssetTags} /> Also use the video's tags</label>
            <div class="row">
              <button type="submit" class="primary" disabled={form.destinations.length === 0}>Save rule</button>
              <button type="button" onclick={() => (showRuleForm = false)}>Cancel</button>
            </div>
          </form>
        {:else}
          <button onclick={() => startRule()}>Add rule</button>
        {/if}

        <fieldset class="unattended">
          <legend>Unattended publishing (skips your approval)</legend>
          <p class="warn">
            Off by default. When on for an account, publications that this collection's rules create for it are
            approved by the rule once the video is rendered and the review shows no problems, then posted at their
            slot without asking you. It never applies to publications you create or edit yourself.
          </p>
          {#each c.unattended as u (u.accountId)}
            <label class="check">
              <input
                type="checkbox"
                checked={u.enabled}
                disabled={!u.available && !u.enabled}
                onchange={(e) => setUnattended(c, u.accountId, e.currentTarget.checked, u.accountName)}
              />
              {u.platformName} · {u.accountName}
              {#if u.unavailableReason}<span class="muted">— {u.unavailableReason}</span>{/if}
            </label>
          {/each}
          {#if c.unattended.length === 0}<p class="muted">No destination accounts yet.</p>{/if}
        </fieldset>
      {:else}
        <p class="muted">Create a collection to add rules.</p>
      {/if}
    </div>
  </div>
</section>
