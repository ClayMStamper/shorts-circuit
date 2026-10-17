import { describe, expect, it } from "vitest";
import {
  actionLabel,
  formatBytes,
  formatDuration,
  formatInZone,
  parseHashtags,
  parseTimecode,
  stateLabel,
  stateTone,
  toLocalInput,
} from "./format";

describe("durations and timecodes", () => {
  it("formats durations as m:ss.s", () => {
    expect(formatDuration(0)).toBe("0:00.0");
    expect(formatDuration(12_340)).toBe("0:12.3");
    expect(formatDuration(75_000)).toBe("1:15.0");
  });

  it("parses timecodes back", () => {
    expect(parseTimecode("1:15.0")).toBe(75_000);
    expect(parseTimecode("12.5")).toBe(12_500);
    expect(parseTimecode("1:00:00")).toBe(3_600_000);
    expect(parseTimecode("abc")).toBeNull();
    expect(parseTimecode("1::2")).toBeNull();
    expect(parseTimecode("-3")).toBeNull();
    expect(parseTimecode(formatDuration(20_000))).toBe(20_000);
  });
});

describe("formatBytes", () => {
  it("uses binary units", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB");
  });
});

describe("timezones", () => {
  it("shows a UTC instant in the user's zone across a DST change", () => {
    // US clocks fall back on 2026-11-01.
    expect(toLocalInput("2026-10-31T13:00:00Z", "America/New_York")).toBe("2026-10-31T09:00");
    expect(toLocalInput("2026-11-02T14:00:00Z", "America/New_York")).toBe("2026-11-02T09:00");
  });

  it("formats with the zone abbreviation", () => {
    expect(formatInZone("2026-11-02T14:00:00Z", "America/New_York")).toMatch(/09:00.*(EST|GMT-5)/);
    expect(formatInZone("2026-07-02T13:00:00Z", "America/New_York")).toMatch(/09:00.*(EDT|GMT-4)/);
  });
});

describe("hashtags", () => {
  it("splits, strips # and de-duplicates case-insensitively", () => {
    expect(parseHashtags("#Bread baking, #bread  #sourdough")).toEqual(["Bread", "baking", "sourdough"]);
    expect(parseHashtags("   ")).toEqual([]);
  });
});

describe("labels", () => {
  it("has a label and tone for every state", () => {
    expect(stateLabel("outcome_unknown")).toBe("Outcome unknown");
    expect(stateTone("published")).toBe("good");
    expect(stateTone("needs_user_action")).toBe("warn");
    expect(stateTone("failed")).toBe("bad");
    expect(actionLabel("confirmPosted")).toBe("Mark as posted");
  });
});
