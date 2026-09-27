import { describe, expect, it } from "vitest";

import { describeShortenStats, loadBalancingHelp, loadBalancingLabel } from "./routing-labels";

describe("loadBalancingLabel", () => {
  it("names the session strategy", () => {
    expect(loadBalancingLabel("session")).toContain("one account per conversation");
  });

  it("falls back to the raw option for unknown strategies", () => {
    expect(loadBalancingLabel("future")).toBe("future");
  });

  it("explains why session routing helps", () => {
    expect(loadBalancingHelp("session")).toContain("prompt cache");
    expect(loadBalancingHelp(undefined)).toBe("");
  });
});

describe("describeShortenStats", () => {
  it("is silent before any Claude Code request", () => {
    expect(describeShortenStats(null)).toBeNull();
    expect(describeShortenStats({ toolsSeen: 0, toolsShortened: 0, bytesBefore: 0, bytesAfter: 0 })).toBeNull();
  });

  it("reports the saving of the last request", () => {
    const text = describeShortenStats({ toolsSeen: 26, toolsShortened: 12, bytesBefore: 44876, bytesAfter: 8000 });
    expect(text).toContain("12 of 26");
    expect(text).toContain("−82%");
  });
});
