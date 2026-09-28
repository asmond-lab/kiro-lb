import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { InfoPanel } from "./components/info-panel";
import type { Overview } from "./types";

function render(version?: Overview["version"], isCheckingUpdates = false, update?: Overview["update"]) {
  return renderToStaticMarkup(
    <InfoPanel
      overview={{
        proxy: { status: "healthy", uptimeSeconds: 100 },
        version,
        update,
        requests24h: 0,
        successes24h: 0,
        averageLatencyMs: 0,
        accounts: { total: 0, initialized: 0 },
        models: 0,
      }}
      accounts={[]}
      isLive
      isCheckingUpdates={isCheckingUpdates}
      onCheckUpdates={() => {}}
      isInstallingUpdate={false}
      onInstallUpdate={() => {}}
    />,
  );
}

describe("InfoPanel version status", () => {
  it("uses the running gateway version and links to an available release", () => {
    const html = render({
      current: "0.2.9",
      latest: "0.2.10",
      status: "update_available",
      releaseUrl: "https://github.com/minpeter/kiro-lb/releases/tag/v0.2.10",
    });
    expect(html).toContain("v0.2.9");
    expect(html).toContain("Update available");
    expect(html).toContain("Latest release: v0.2.10");
    expect(html).toContain('href="https://github.com/minpeter/kiro-lb/releases/tag/v0.2.10"');
    expect(html).not.toContain("Latest version");
  });

  it.each([
    ["latest", "Latest version"],
    ["ahead", "Newer than latest release"],
    ["checking", "Checking for updates…"],
    ["unavailable", "Could not check for updates"],
  ] as const)("displays %s without claiming an update is available", (status, label) => {
    const html = render({ current: "0.2.1", latest: null, status, releaseUrl: null });
    expect(html).toContain("v0.2.1");
    expect(html).toContain(label);
    expect(html).not.toContain("Update available");
    if (status !== "latest") expect(html).not.toContain("Latest version");
  });

  it("does not invent a version or claim latest when an older backend omits it", () => {
    const html = render();
    expect(html).toContain("Could not check for updates");
    expect(html).not.toContain("Latest version");
    expect(html).not.toContain("v0.1.0");
  });

  it("keeps manual checking available even with a cached initial checking status", () => {
    const html = render({ current: "0.2.1", latest: null, status: "checking", releaseUrl: null });
    expect(html).toContain("Check now");
    expect(html).not.toContain('disabled=""');
  });

  it("disables the button and shows checking instead of a stale latest claim during a manual check", () => {
    const html = render({ current: "0.2.1", latest: "0.2.1", status: "latest", releaseUrl: null }, true);
    expect(html).toContain("Checking for updates…");
    expect(html).toContain('disabled=""');
    expect(html).toContain('aria-busy="true"');
    expect(html).not.toContain("Latest version");
  });

  it("offers installation only for supported deployments with a newer release", () => {
    const version: Overview["version"] = { current: "0.2.1", latest: "0.2.2", status: "update_available", releaseUrl: null };
    const update: Overview["update"] = { status: "idle", version: null, error: null, disabledReason: null };
    expect(render(version, false, update)).toContain("Update and restart");
    expect(render({ ...version, status: "latest" }, false, update)).not.toContain("Update and restart");
    const container = render(version, false, { ...update, disabledReason: "container" });
    expect(container).not.toContain("Update and restart");
    expect(container).toContain("recreate the container");
  });
});
