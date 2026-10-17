import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { invoke } from "@tauri-apps/api/core";
import { setInvoke } from "../lib/api";
import type { AccountView, CollectionView } from "../lib/types";
import Collections from "./Collections.svelte";

const account: AccountView = {
  id: "acc1",
  platform: "youtube_shorts",
  platformName: "YouTube Shorts",
  displayName: "Channel",
  automatic: false,
  missingReason: "No platform API connection is configured for this account.",
  nextAction: "export_package",
  manualHandoff: true,
};

function collection(overrides: Partial<CollectionView> = {}): CollectionView {
  return {
    id: "c1",
    name: "From phone",
    memberCount: 2,
    rules: [],
    unattended: [
      {
        accountId: "acc1",
        accountName: "Channel",
        platformName: "YouTube Shorts",
        enabled: false,
        available: false,
        unavailableReason: "Publishes through export packages, so approval is always required.",
        enabledAt: null,
      },
    ],
    watchFolderTarget: true,
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  setInvoke(invoke);
});

describe("Collections", () => {
  it("labels unattended publishing clearly and keeps it off for export-package accounts", async () => {
    setInvoke(
      vi.fn(async (cmd: string) => {
        if (cmd === "list_collections") return [collection()] as never;
        if (cmd === "list_accounts") return [account] as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(Collections, { revision: 0 });
    expect(await screen.findByText("Unattended publishing (skips your approval)")).toBeTruthy();
    const toggle = screen.getByRole("checkbox", { name: /YouTube Shorts · Channel/ }) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    expect(toggle.disabled).toBe(true);
    expect(screen.getByText(/approval is always required/)).toBeTruthy();
  });

  it("creates a rule that submits publications for approval by default", async () => {
    const calls: Array<[string, unknown]> = [];
    setInvoke(
      vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        calls.push([cmd, args]);
        if (cmd === "list_collections") return [collection()] as never;
        if (cmd === "list_accounts") return [account] as never;
        if (cmd === "create_rule")
          return collection({
            rules: [
              {
                id: "r1",
                name: "Cross-post",
                enabled: true,
                destinations: [{ accountId: "acc1", accountName: "Channel", platformName: "YouTube Shorts", unattended: false }],
                output: "awaiting_approval",
                captionTemplate: "{title}",
                hashtags: ["shorts"],
                includeAssetTags: true,
                firedCount: 0,
              },
            ],
          }) as never;
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    );
    render(Collections, { revision: 0 });
    await fireEvent.click(await screen.findByRole("button", { name: "Add rule" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "YouTube Shorts · Channel" }));
    await fireEvent.input(screen.getByPlaceholderText("#shorts #recipe"), { target: { value: "#shorts" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save rule" }));

    const create = calls.find(([cmd]) => cmd === "create_rule");
    expect(create?.[1]).toEqual({
      collectionId: "c1",
      rule: {
        name: "Cross-post",
        enabled: true,
        destinations: ["acc1"],
        output: "awaiting_approval",
        captionTemplate: "{title}",
        hashtags: ["#shorts"],
        includeAssetTags: true,
      },
    });
    expect(await screen.findByTestId("rule")).toBeTruthy();
    expect(screen.getByText(/submitted for your approval/)).toBeTruthy();
  });
});
