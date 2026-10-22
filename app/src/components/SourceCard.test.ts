import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { Preferences } from "../lib/types";
import SourceCard from "./SourceCard.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const prefs: Preferences = {
  timezone: "America/Chicago",
  overduePolicy: { kind: "catch_up" } as Preferences["overduePolicy"],
  watchFolder: "D:\Shorts_Ingress",
  exportDir: null,
  postingSlots: [],
  watchCollection: null,
  watchCheckedAt: null,
  downloadsFolder: null,
};

describe("SourceCard downloads", () => {
  it("turns on picking up TikTok downloads from the OS downloads folder", async () => {
    const calls: Array<[string, unknown]> = [];
    let current = prefs;
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "preferences") return current as never;
        if (cmd === "default_downloads_folder") return "C:\Users\clay\Downloads" as never;
        if (cmd === "update_preferences") {
          current = { ...current, ...(args!.update as Partial<Preferences>) };
          return current as never;
        }
        return { kind: "off" } as never;
      }) as never,
    );
    render(SourceCard, {});
    const toggle = (await screen.findByLabelText("Also pick up originals I download from TikTok")) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    await fireEvent.click(toggle);
    await vi.waitFor(() =>
      expect(calls).toContainEqual(["update_preferences", { update: { downloadsFolder: "C:\Users\clay\Downloads" } }]),
    );
    expect(await screen.findByText("C:\Users\clay\Downloads")).toBeTruthy();
  });
});
