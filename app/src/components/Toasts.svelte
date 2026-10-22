<script lang="ts" module>
  type Tone = "good" | "info" | "bad";
  interface Toast {
    id: number;
    text: string;
    tone: Tone;
  }

  let items = $state<Toast[]>([]);
  let next = 0;

  export function toast(text: string, tone: Tone = "info") {
    const id = next++;
    items.push({ id, text, tone });
    setTimeout(() => {
      items = items.filter((t) => t.id !== id);
    }, tone === "bad" ? 8000 : 4000);
  }

  export function errorToast(e: unknown) {
    const message = e && typeof e === "object" && "message" in e ? String(e.message) : String(e);
    toast(message, "bad");
  }
</script>

<div class="toasts" aria-live="polite">
  {#each items as t (t.id)}
    <div class="toast {t.tone}">{t.text}</div>
  {/each}
</div>
