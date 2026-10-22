import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { SnapProfileView, SnapStatus } from "../lib/types";
import SnapchatSettings from "./SnapchatSettings.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const DEFAULT_REDIRECT = "http://127.0.0.1:47615/snapchat/callback";
const PAGE_REDIRECT = "https://claymstamper.github.io/shorts-circuit/snap-callback.html";

const unconfigured: SnapStatus = {
  clientConfigured: false,
  redirectUri: DEFAULT_REDIRECT,
  returnMode: "loopback",
  nextStep: "Create an OAuth app in Snapchat Ads Manager…",
  connecting: false,
  awaitingCode: false,
  profiles: [],
};

const profile: SnapProfileView = {
  accountId: "acc1",
  displayName: "Test Kitchen",
  connected: true,
  needsReconnect: false,
  lastError: null,
  notes: ["Spotlight posts are public."],
  connectedAt: "2026-10-30T12:00:00Z",
};

describe("SnapchatSettings", () => {
  it("saves the OAuth app with the default redirect address and never shows the secret again", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "snapchat_status") return unconfigured as never;
        if (cmd === "snapchat_set_client") return { ...unconfigured, clientConfigured: true, nextStep: null } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(SnapchatSettings, {});
    const id = (await screen.findByLabelText("Client ID")) as HTMLInputElement;
    const secret = screen.getByLabelText("Client secret") as HTMLInputElement;
    const redirect = screen.getByLabelText("Redirect address") as HTMLInputElement;
    expect(secret.type).toBe("password");
    expect(redirect.value).toBe(DEFAULT_REDIRECT);
    expect(screen.getByTestId("snap-redirect-hint").textContent).toContain("Register exactly this address");
    expect((screen.getByRole("button", { name: "Connect a Snapchat public profile…" }) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(id, { target: { value: "snap-client-id" } });
    await fireEvent.input(secret, { target: { value: "snap-secret-value" } });
    await fireEvent.submit(screen.getByRole("form", { name: "Snap OAuth app" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "snapchat_set_client",
        { clientId: "snap-client-id", clientSecret: "snap-secret-value", redirectUri: DEFAULT_REDIRECT },
      ]),
    );
    expect((await screen.findByTestId("snap-client-saved")).textContent).toContain(DEFAULT_REDIRECT);
    expect(document.body.innerHTML).not.toContain("snap-secret-value");
  });

  it("connects in paste mode with the text from the redirect page", async () => {
    const calls: Array<[string, unknown]> = [];
    const configured: SnapStatus = {
      ...unconfigured,
      clientConfigured: true,
      nextStep: null,
      redirectUri: PAGE_REDIRECT,
      returnMode: "paste",
    };
    let current = configured;
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "snapchat_status") return current as never;
        if (cmd === "snapchat_connect") {
          current = { ...configured, awaitingCode: true };
          return undefined as never;
        }
        if (cmd === "snapchat_finish_with_code") {
          current = { ...configured, profiles: [profile] };
          return current as never;
        }
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(SnapchatSettings, {});
    await fireEvent.click(await screen.findByRole("button", { name: "Connect a Snapchat public profile…" }));
    const field = (await screen.findByLabelText("Paste the text from the page")) as HTMLInputElement;
    expect(calls).toContainEqual(["snapchat_connect", { accountId: null }]);
    expect(calls.map(([c]) => c)).not.toContain("snapchat_connect_progress");
    await fireEvent.input(field, { target: { value: "  state-abcdefghijklmnopqrstuvwxyz.code-123  " } });
    await fireEvent.submit(screen.getByRole("form", { name: "Snap sign-in code" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "snapchat_finish_with_code",
        { pasted: "  state-abcdefghijklmnopqrstuvwxyz.code-123  " },
      ]),
    );
    expect(await screen.findByTestId("snap-profile")).toBeTruthy();
    expect(screen.queryByLabelText("Paste the text from the page")).toBeNull();
  });

  it("marks a profile that needs reconnecting and offers Reconnect, Check connection and Disconnect", async () => {
    const calls: Array<[string, unknown]> = [];
    const expired: SnapStatus = {
      ...unconfigured,
      clientConfigured: true,
      nextStep: null,
      profiles: [{ ...profile, needsReconnect: true, lastError: "Snap refused the profile's sign-in" }],
    };
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "snapchat_status") return expired as never;
        if (cmd === "snapchat_check_connection") return expired as never;
        if (cmd === "snapchat_connect") return undefined as never;
        if (cmd === "snapchat_connect_progress") return { kind: "pending" } as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(SnapchatSettings, {});
    const item = await screen.findByTestId("snap-profile");
    expect(item.textContent).toContain("needs reconnecting");
    expect(item.textContent).toContain("Snap refused the profile's sign-in");
    expect(item.textContent).toContain("Spotlight posts are public.");
    expect(screen.getByRole("button", { name: "Disconnect" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Check connection" }));
    await vi.waitFor(() => expect(calls).toContainEqual(["snapchat_check_connection", { accountId: "acc1" }]));
    await fireEvent.click(screen.getByRole("button", { name: "Reconnect Test Kitchen…" }));
    await vi.waitFor(() => expect(calls).toContainEqual(["snapchat_connect", { accountId: "acc1" }]));
    expect(await screen.findByText("Finish signing in to Snapchat in your browser…")).toBeTruthy();
  });
});
