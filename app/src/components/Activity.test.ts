import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import Activity from "./Activity.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

describe("Activity", () => {
  it("reloads the list when Refresh is pressed", async () => {
    let loads = 0;
    let finish: () => void = () => {};
    setInvoke(
      vi.fn(async (cmd: string) => {
        if (cmd !== "activity") return null as never;
        loads += 1;
        // Hold the second load open to see the button's busy state.
        if (loads === 2) await new Promise<void>((r) => (finish = r));
        return { items: [] } as never;
      }) as never,
    );
    render(Activity, { onConnections: () => {} });
    await screen.findByText("No videos yet");
    expect(loads).toBe(1);

    await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    const busy = await screen.findByRole("button", { name: "Refreshing…" });
    expect((busy as HTMLButtonElement).disabled).toBe(true);
    expect(loads).toBe(2);

    finish();
    await screen.findByRole("button", { name: "Refresh" });
  });
});
