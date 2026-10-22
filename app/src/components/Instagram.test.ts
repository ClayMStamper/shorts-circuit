import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { InstagramAccount, InstagramPages, InstagramStatus } from "../lib/types";
import InstagramSettings from "./InstagramSettings.svelte";
import Toasts from "./Toasts.svelte";

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

const empty: InstagramStatus = {
  accounts: [],
  nextStep: "Link your Instagram professional account to a Facebook Page…",
};

const account: InstagramAccount = {
  accountId: "acc1",
  displayName: "@testkitchen",
  connected: true,
  needsReconnect: false,
  login: "facebook_login",
  canPublish: true,
  pageName: "Timeless Way Interactive",
  username: "testkitchen",
  lastError: null,
  notes: ["Instagram allows 100 posts through the API per account in 24 hours."],
  connectedAt: "2026-10-30T12:00:00Z",
};

const connected: InstagramStatus = { nextStep: null, accounts: [account] };

const onePage: InstagramPages = {
  warning: null,
  pages: [{ pageId: "p1", pageName: "Timeless Way Interactive", instagramUsername: "testkitchen" }],
};

function mock(handlers: Record<string, (args?: Record<string, unknown>) => unknown>) {
  const calls: Array<[string, unknown]> = [];
  setInvoke(
    vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      const h = handlers[cmd];
      if (!h) throw new Error(`unexpected ${cmd}`);
      return h(args) as never;
    }) as never,
  );
  return calls;
}

async function paste(value: string) {
  const field = screen.getByLabelText("Long-lived user access token") as HTMLInputElement;
  expect(field.type).toBe("password");
  await fireEvent.input(field, { target: { value } });
  await fireEvent.submit(screen.getByRole("form", { name: "Instagram access token" }));
  return field;
}

describe("InstagramSettings", () => {
  it("finds the only linked Page and connects it straight away, never showing the token again", async () => {
    const calls = mock({
      instagram_status: () => empty,
      instagram_find_pages: () => onePage,
      instagram_connect: () => connected,
    });
    render(InstagramSettings, {});
    expect(await screen.findByTestId("instagram-next-step")).toBeTruthy();
    expect(screen.getByText(/Graph API Explorer/)).toBeTruthy();
    expect(screen.getByText(/could later publish Facebook Reels/)).toBeTruthy();
    const field = await paste("EAA-secret-user-token");
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "instagram_connect",
        { accessToken: "EAA-secret-user-token", login: "facebook_login", pageId: "p1", accountId: null },
      ]),
    );
    expect(calls).toContainEqual(["instagram_find_pages", { accessToken: "EAA-secret-user-token" }]);
    expect(await screen.findByTestId("instagram-account")).toBeTruthy();
    expect(screen.getByText("@testkitchen")).toBeTruthy();
    expect(screen.getByText(/Page: Timeless Way Interactive/)).toBeTruthy();
    expect(screen.getByText(/Publishes as @testkitchen through the Facebook Page Timeless Way Interactive/)).toBeTruthy();
    expect(field.value).toBe("");
    expect(document.body.innerHTML).not.toContain("EAA-secret-user-token");
  });

  it("lets the owner pick among several Pages and explains the unlinked ones", async () => {
    const calls = mock({
      instagram_status: () => empty,
      instagram_find_pages: () => ({
        warning: null,
        pages: [
          { pageId: "p1", pageName: "Timeless Way Interactive", instagramUsername: "testkitchen" },
          { pageId: "p2", pageName: "Second studio", instagramUsername: "studio2" },
          { pageId: "p0", pageName: "Old blog", instagramUsername: null },
        ],
      }),
      instagram_connect: () => connected,
    });
    render(InstagramSettings, {});
    await screen.findByTestId("instagram-next-step");
    await paste("EAA-user-token-two-pages");
    expect(await screen.findByTestId("instagram-pages")).toBeTruthy();
    expect(screen.getByText(/no linked Instagram account/)).toBeTruthy();
    const old = screen.getByRole("radio", { name: /Old blog/ }) as HTMLInputElement;
    expect(old.disabled).toBe(true);
    const connect = screen.getByRole("button", { name: "Connect" }) as HTMLButtonElement;
    expect(connect.disabled).toBe(true);
    await fireEvent.click(screen.getByRole("radio", { name: /Second studio/ }));
    await fireEvent.submit(screen.getByRole("form", { name: "Instagram access token" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "instagram_connect",
        { accessToken: "EAA-user-token-two-pages", login: "facebook_login", pageId: "p2", accountId: null },
      ]),
    );
  });

  it("shows the next step when a token is refused", async () => {
    mock({
      instagram_status: () => empty,
      instagram_find_pages: () => {
        throw { kind: "invalid", message: "The token is missing the instagram_content_publish permission." };
      },
    });
    render(Toasts, {});
    render(InstagramSettings, {});
    await screen.findByTestId("instagram-next-step");
    const field = await paste("EAA-token-without-publishing");
    expect(await screen.findByText(/missing the instagram_content_publish permission/)).toBeTruthy();
    expect(field.value).toBe("");
  });

  it("says plainly that Instagram login can't upload files", async () => {
    const calls = mock({
      instagram_status: () => empty,
      instagram_connect: () => ({
        nextStep: empty.nextStep,
        accounts: [{ ...account, login: "instagram_login", canPublish: false, needsReconnect: true, pageName: null }],
      }),
    });
    render(InstagramSettings, {});
    await screen.findByTestId("instagram-next-step");
    await fireEvent.click(screen.getByRole("radio", { name: /Instagram login/ }));
    expect(screen.getByTestId("instagram-login-limit").textContent).toContain("can't upload files directly");
    await fireEvent.input(screen.getByLabelText("Instagram login token"), { target: { value: "IGAA-old-token" } });
    await fireEvent.submit(screen.getByRole("form", { name: "Instagram access token" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "instagram_connect",
        { accessToken: "IGAA-old-token", login: "instagram_login", pageId: null, accountId: null },
      ]),
    );
  });

  it("asks an account connected with Instagram login to reconnect with Facebook login", async () => {
    const calls = mock({
      instagram_status: () => ({
        nextStep: empty.nextStep,
        accounts: [
          {
            ...account,
            login: "instagram_login",
            canPublish: false,
            needsReconnect: true,
            pageName: null,
            lastError: "Connected with Instagram login, which can't upload files directly.",
          },
        ],
      }),
      instagram_find_pages: () => onePage,
      instagram_connect: () => connected,
    });
    render(InstagramSettings, {});
    expect(await screen.findByText("reconnect with Facebook login")).toBeTruthy();
    expect(screen.getByText(/can't upload files directly/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Reconnect @testkitchen with Facebook login…" }));
    expect(screen.getByText("Reconnect @testkitchen")).toBeTruthy();
    await paste("EAA-new-user-token");
    await vi.waitFor(() =>
      expect(calls).toContainEqual([
        "instagram_connect",
        { accessToken: "EAA-new-user-token", login: "facebook_login", pageId: "p1", accountId: "acc1" },
      ]),
    );
  });
});
