import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { InstagramAccount, InstagramStatus } from "../lib/types";
import InstagramSettings from "./InstagramSettings.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const empty: InstagramStatus = {
  accounts: [],
  nextStep: "Create a Meta app with the Instagram API…",
};

const account: InstagramAccount = {
  accountId: "acc1",
  displayName: "@testkitchen",
  connected: true,
  needsReconnect: false,
  lastError: null,
  notes: ["Instagram allows 100 posts through the API per account in 24 hours."],
  connectedAt: "2026-10-30T12:00:00Z",
  tokenExpiresAt: "2026-12-29T12:00:00Z",
  renewedAt: null,
};

const connected: InstagramStatus = { nextStep: null, accounts: [account] };

describe("InstagramSettings", () => {
  it("connects with a pasted token and never shows it again", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "instagram_status") return empty as never;
        if (cmd === "instagram_connect") return connected as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(InstagramSettings, {});
    expect(await screen.findByTestId("instagram-next-step")).toBeTruthy();
    const field = screen.getByLabelText("Access token") as HTMLInputElement;
    expect(field.type).toBe("password");
    await fireEvent.input(field, { target: { value: "IGAA-secret-token" } });
    await fireEvent.submit(screen.getByRole("form", { name: "Instagram access token" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual(["instagram_connect", { accessToken: "IGAA-secret-token", accountId: null }]),
    );
    expect(await screen.findByTestId("instagram-account")).toBeTruthy();
    expect(screen.getByText("@testkitchen")).toBeTruthy();
    expect(field.value).toBe("");
    expect(document.body.innerHTML).not.toContain("IGAA-secret-token");
  });

  it("offers a reconnect for an account whose sign-in ran out", async () => {
    const calls: Array<[string, unknown]> = [];
    const expired: InstagramStatus = {
      nextStep: null,
      accounts: [
        {
          ...account,
          needsReconnect: true,
          lastError: "the Instagram sign-in ran out",
        },
      ],
    };
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "instagram_status") return expired as never;
        if (cmd === "instagram_connect") return connected as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(InstagramSettings, {});
    expect(await screen.findByText("needs reconnecting")).toBeTruthy();
    expect(screen.getByText("the Instagram sign-in ran out")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Reconnect @testkitchen…" }));
    expect(screen.getByText("Reconnect @testkitchen")).toBeTruthy();
    await fireEvent.input(screen.getByLabelText("Access token"), { target: { value: "IGAA-new-token" } });
    await fireEvent.submit(screen.getByRole("form", { name: "Instagram access token" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual(["instagram_connect", { accessToken: "IGAA-new-token", accountId: "acc1" }]),
    );
  });
});
