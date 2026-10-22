import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { LinkedInPage, LinkedInStatus } from "../lib/types";
import LinkedInSettings from "./LinkedInSettings.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const empty: LinkedInStatus = { pages: [], nextStep: "Create a LinkedIn developer app…" };

const page: LinkedInPage = {
  accountId: "acc1",
  displayName: "Timeless Way Interactive",
  connected: true,
  needsReconnect: false,
  expiresSoon: false,
  lastError: null,
  notes: ["Posts publicly as Timeless Way Interactive."],
  connectedAt: "2026-10-30T12:00:00Z",
  tokenExpiresAt: "2026-12-29T12:00:00Z",
};

describe("LinkedInSettings", () => {
  it("finds the token's pages, picks one and connects without echoing the token", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "linkedin_status") return empty as never;
        if (cmd === "linkedin_find_pages")
          return [
            { urn: "urn:li:organization:1", name: "Timeless Way Interactive" },
            { urn: "urn:li:organization:2", name: "Side project" },
          ] as never;
        if (cmd === "linkedin_connect") return { pages: [page], nextStep: null } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(LinkedInSettings, {});
    expect(await screen.findByTestId("linkedin-next-step")).toBeTruthy();
    const field = screen.getByLabelText("Access token") as HTMLInputElement;
    expect(field.type).toBe("password");
    await fireEvent.input(field, { target: { value: "AQV-secret-token" } });
    await fireEvent.click(screen.getByRole("button", { name: "Find my pages" }));
    const select = (await screen.findByLabelText("Page")) as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "urn:li:organization:1" } });
    await fireEvent.submit(screen.getByRole("form", { name: "LinkedIn access token" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "linkedin_connect",
        { accessToken: "AQV-secret-token", page: "urn:li:organization:1", accountId: null },
      ]),
    );
    expect(await screen.findByTestId("linkedin-page")).toBeTruthy();
    expect(field.value).toBe("");
    expect(document.body.innerHTML).not.toContain("AQV-secret-token");
  });

  it("warns before the token runs out and offers a reconnect", async () => {
    setInvoke(
      vi.fn(async (cmd: string) => {
        if (cmd === "linkedin_status")
          return { pages: [{ ...page, expiresSoon: true, tokenExpiresAt: "2026-11-02T12:00:00Z" }], nextStep: null } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(LinkedInSettings, {});
    expect(await screen.findByTestId("expires-soon")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Reconnect Timeless Way Interactive…" }));
    expect(screen.getByText("Reconnect Timeless Way Interactive")).toBeTruthy();
  });
});
