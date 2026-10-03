import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

describe("alerts", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("window", globalThis);
    vi.resetModules();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("fades an alert out and removes it after its lifetime", async () => {
    const alerts = await import("./alerts");
    alerts.pushAlert({ tone: "success", text: "saved" });
    vi.advanceTimersByTime(3999);
    expect(alerts.getSnapshot()).toMatchObject([{ text: "saved", leaving: false }]);
    vi.advanceTimersByTime(1);
    expect(alerts.getSnapshot()).toMatchObject([{ text: "saved", leaving: true }]);
    vi.advanceTimersByTime(alerts.ALERT_FADE_MS);
    expect(alerts.getSnapshot()).toEqual([]);
  });

  it("refreshes an identical active alert instead of stacking a duplicate", async () => {
    const alerts = await import("./alerts");
    const first = alerts.pushAlert({ tone: "error", error: "Invalid password" });
    const second = alerts.pushAlert({ tone: "error", error: "Invalid password" });
    expect(second).toBe(first);
    const third = alerts.pushAlert({ tone: "error", error: "Not Found" });
    expect(third).not.toBe(first);
  });
});
