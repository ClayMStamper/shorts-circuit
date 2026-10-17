import { afterEach, describe, expect, it, vi } from "vitest";
import { api, setInvoke, toAppError } from "./api";
import { invoke } from "@tauri-apps/api/core";

afterEach(() => setInvoke(invoke));

describe("api", () => {
  it("passes camelCase arguments to the command", async () => {
    const fake = vi.fn().mockResolvedValue({ id: "p1" });
    setInvoke(fake);
    await api.approve("p1", 3);
    expect(fake).toHaveBeenCalledWith("approve", { id: "p1", revision: 3 });
    await api.schedule("p1", { kind: "local", local: "2026-11-02T09:00", timezone: "Europe/Paris" });
    expect(fake).toHaveBeenLastCalledWith("schedule", {
      id: "p1",
      input: { kind: "local", local: "2026-11-02T09:00", timezone: "Europe/Paris" },
    });
    await api.importFiles(["/a.mp4"]);
    expect(fake).toHaveBeenLastCalledWith("import_files", { paths: ["/a.mp4"], into: null });
  });

  it("normalizes rejections to AppError", async () => {
    setInvoke(vi.fn().mockRejectedValue({ kind: "transition", message: "not allowed" }));
    await expect(api.monitor()).rejects.toEqual({ kind: "transition", message: "not allowed" });
    setInvoke(vi.fn().mockRejectedValue("plain string"));
    await expect(api.monitor()).rejects.toEqual({ kind: "error", message: "plain string" });
    expect(toAppError(new Error("boom"))).toEqual({ kind: "error", message: "boom" });
  });
});
