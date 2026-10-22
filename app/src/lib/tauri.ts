// Thin wrappers over Tauri APIs that degrade gracefully outside the desktop shell (tests,
// `vite dev` in a browser).

import { convertFileSrc } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppEvent } from "./types";

export function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** A URL the webview can load for a local file (video preview, thumbnails). */
export function fileUrl(path: string): string {
  // Already a URL (the browser preview's mock data).
  if (/^(data:|https?:|blob:)/.test(path)) return path;
  if (!inTauri()) return `file://${path}`;
  return convertFileSrc(path);
}

export async function onAppEvent(handler: (event: AppEvent) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  return listen<AppEvent>("app-event", (e) => handler(e.payload));
}

/** Native file drops onto the window. */
export async function onFileDrop(handlers: {
  over: (hovering: boolean) => void;
  drop: (paths: string[]) => void;
}): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((event) => {
    const p = event.payload;
    if (p.type === "enter" || p.type === "over") handlers.over(true);
    else if (p.type === "leave") handlers.over(false);
    else if (p.type === "drop") {
      handlers.over(false);
      handlers.drop(p.paths);
    }
  });
}

export async function pickFiles(options: {
  multiple: boolean;
  extensions: string[];
  name: string;
}): Promise<string[]> {
  if (!inTauri()) return [];
  const { open } = await import("@tauri-apps/plugin-dialog");
  const result = await open({
    multiple: options.multiple,
    directory: false,
    filters: [{ name: options.name, extensions: options.extensions }],
  });
  if (result === null) return [];
  return Array.isArray(result) ? result : [result];
}

export async function pickFolder(): Promise<string | null> {
  if (!inTauri()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const result = await open({ directory: true, multiple: false });
  return typeof result === "string" ? result : null;
}

export const VIDEO_EXTENSIONS = ["mp4", "mov", "m4v", "webm", "mkv", "avi"];
