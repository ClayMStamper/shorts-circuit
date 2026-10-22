<script lang="ts">
  // Simplified platform marks as inline SVG (no network, no icon font). Decorative unless a
  // label is given.
  import type { Platform } from "../../lib/types";

  type Kind = Platform | "tiktok" | "folder";
  let { platform, size = 28, label }: { platform: Kind; size?: number; label?: string } = $props();
</script>

<svg
  class="platform-icon"
  width={size}
  height={size}
  viewBox="0 0 32 32"
  role={label ? "img" : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : "true"}
>
  {#if platform === "youtube_shorts"}
    <rect width="32" height="32" rx="7" fill="#FF0033" />
    <path d="M12.5 10.5v11l9.5-5.5z" fill="#fff" />
  {:else if platform === "instagram_reels"}
    <defs>
      <linearGradient id="ig-grad" x1="0" y1="32" x2="32" y2="0">
        <stop offset="0" stop-color="#FEDA75" />
        <stop offset="0.35" stop-color="#FA7E1E" />
        <stop offset="0.65" stop-color="#D62976" />
        <stop offset="1" stop-color="#4F5BD5" />
      </linearGradient>
    </defs>
    <rect width="32" height="32" rx="8" fill="url(#ig-grad)" />
    <rect x="8" y="8" width="16" height="16" rx="5" fill="none" stroke="#fff" stroke-width="2.2" />
    <circle cx="16" cy="16" r="3.8" fill="none" stroke="#fff" stroke-width="2.2" />
    <circle cx="21.2" cy="10.8" r="1.2" fill="#fff" />
  {:else if platform === "snapchat_spotlight"}
    <rect width="32" height="32" rx="7" fill="#FFFC00" />
    <path
      d="M16 7.5c-3.3 0-5.4 2.6-5.4 5.4v2.2l-1.6-.4c-.6 0-.9.7-.4 1.1l1.8 1.1c-.6 1.6-1.8 2.9-3.4 3.4-.4.2-.3.7.1.8l1.6.4.4 1.2 2.1-.1c.9 0 1.7 1.7 4.8 1.7s3.9-1.7 4.8-1.7l2.1.1.4-1.2 1.6-.4c.4-.1.5-.6.1-.8-1.6-.5-2.8-1.8-3.4-3.4l1.8-1.1c.5-.4.2-1.1-.4-1.1l-1.6.4v-2.2c0-2.8-2.1-5.4-5.4-5.4z"
      fill="#fff"
      stroke="#000"
      stroke-width="1.1"
      stroke-linejoin="round"
    />
  {:else if platform === "linkedin"}
    <rect width="32" height="32" rx="7" fill="#0A66C2" />
    <rect x="8" y="13" width="3.6" height="11" fill="#fff" />
    <circle cx="9.8" cy="9.6" r="2.1" fill="#fff" />
    <path d="M14.5 13h3.4v1.6c.6-1 1.9-1.9 3.7-1.9 3 0 3.9 1.9 3.9 4.8V24h-3.6v-5.7c0-1.5-.4-2.5-1.8-2.5s-2 1-2 2.5V24h-3.6z" fill="#fff" />
  {:else if platform === "tiktok"}
    <rect width="32" height="32" rx="7" fill="#010101" />
    <path d="M18.6 7h3c.3 2.2 1.6 3.6 3.9 3.8v3.1c-1.5 0-2.8-.5-3.9-1.2v6.6c0 3.6-2.7 6-6 6-3.2 0-5.6-2.5-5.6-5.6 0-3.4 2.9-6 6.4-5.5v3.2c-1.6-.4-3.2.6-3.2 2.3 0 1.3 1 2.4 2.4 2.4 1.5 0 2.9-1 2.9-3z" fill="#25F4EE" transform="translate(-0.8 0.6)" />
    <path d="M18.6 7h3c.3 2.2 1.6 3.6 3.9 3.8v3.1c-1.5 0-2.8-.5-3.9-1.2v6.6c0 3.6-2.7 6-6 6-3.2 0-5.6-2.5-5.6-5.6 0-3.4 2.9-6 6.4-5.5v3.2c-1.6-.4-3.2.6-3.2 2.3 0 1.3 1 2.4 2.4 2.4 1.5 0 2.9-1 2.9-3z" fill="#FE2C55" transform="translate(0.8 -0.4)" />
    <path d="M18.6 7h3c.3 2.2 1.6 3.6 3.9 3.8v3.1c-1.5 0-2.8-.5-3.9-1.2v6.6c0 3.6-2.7 6-6 6-3.2 0-5.6-2.5-5.6-5.6 0-3.4 2.9-6 6.4-5.5v3.2c-1.6-.4-3.2.6-3.2 2.3 0 1.3 1 2.4 2.4 2.4 1.5 0 2.9-1 2.9-3z" fill="#fff" />
  {:else}
    <rect width="32" height="32" rx="7" fill="#6B7280" />
    <path d="M7 11.5c0-1 .8-1.8 1.8-1.8h4.4l1.8 2h8.2c1 0 1.8.8 1.8 1.8v8.2c0 1-.8 1.8-1.8 1.8H8.8c-1 0-1.8-.8-1.8-1.8z" fill="#fff" />
  {/if}
</svg>
