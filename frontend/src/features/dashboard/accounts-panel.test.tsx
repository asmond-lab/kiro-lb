import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AccountsPanel } from "./components/accounts-panel";
import type { Account } from "./types";

const mockAccount: Account = {
  id: "acc_test12345",
  initialized: true,
  routingState: "available",
  eligibleInSeconds: 0,
  requests: 10,
  failures: 0,
  cooldownSeconds: 0,
  deletable: false,
};

const deletableAccount: Account = {
  id: "acc_deletable1",
  initialized: true,
  routingState: "available",
  eligibleInSeconds: 0,
  requests: 10,
  failures: 0,
  cooldownSeconds: 0,
  deletable: true,
};

const nonDeletableAccount: Account = {
  id: "acc_nondeletable2",
  initialized: true,
  routingState: "available",
  eligibleInSeconds: 0,
  requests: 5,
  failures: 0,
  cooldownSeconds: 0,
  deletable: false,
};

describe("AccountsPanel", () => {
  it("renders unchanged AccountsPanel with a known account id", () => {
    const html = renderToString(
      <AccountsPanel accounts={[mockAccount]} isLoading={false} />
    );
    expect(html).toContain("acc_test12345");
  });

  it("renders delete trigger only for deletable accounts", () => {
    const html = renderToString(
      <AccountsPanel accounts={[deletableAccount, nonDeletableAccount]} isLoading={false} />
    );
    expect(html).toContain('aria-label="Delete account acc_deletable1"');
    expect(html).not.toContain('aria-label="Delete account acc_nondeletable2"');
  });

  it("disables delete trigger while mutating", () => {
    const html = renderToString(
      <AccountsPanel accounts={[deletableAccount]} isLoading={false} isMutating={true} />
    );
    expect(html).toContain('aria-label="Delete account acc_deletable1"');
    expect(html).toContain('disabled=""');
  });

  it("keeps a paused account's snapshot visible and offers a clear resume action", () => {
    const paused: Account = {
      ...deletableAccount,
      enabled: false,
      routingState: "disabled",
      requests: 41,
      failures: 3,
      sessions: 1,
      usage: {
        email: "paused@example.com",
        subscriptionTitle: "Kiro Pro",
        usagePercent: 42,
        currentUsage: 420,
        usageLimit: 1000,
      },
    };
    const html = renderToString(
      <AccountsPanel accounts={[paused]} isLoading={false} onToggleAccount={() => undefined} />
    );

    expect(html).toContain("Paused accounts");
    expect(html).toContain("last known details are kept");
    expect(html).toContain("paused@example.com");
    expect(html).toContain("Kiro Pro");
    expect(html).toContain("42.00%");
    expect(html).toContain(">41<");
    expect(html).toContain(">3<");
    expect(html).toContain(">Resume<");
  });
});

describe("banned account grouping", () => {
  const banned: Account = {
    ...deletableAccount,
    id: "acc_banned_first",
    enabled: true,
    routingState: "suspended",
    requests: 73,
    failures: 5,
    usage: {
      email: "banned@example.com",
      subscriptionTitle: "Kiro Pro",
      usagePercent: 37,
      currentUsage: 370,
      usageLimit: 1000,
    },
  };
  const paused: Account = { ...mockAccount, id: "acc_paused", enabled: false, routingState: "disabled" };
  const otherStates: Account[] = ["rate_limited", "auth_dead", "quota_depleted", "cooling_down", "quota_exhausted", "uninitialized"].map(
    (state) => ({ ...mockAccount, id: `acc_${state}`, routingState: state as Account["routingState"] }),
  );

  it("groups only suspended accounts last in both card and table layouts, preserving order within groups", () => {
    const secondBanned = { ...banned, id: "acc_banned_second" };
    const html = renderToString(
      <AccountsPanel accounts={[banned, paused, ...otherStates, secondBanned, mockAccount]} isLoading={false} />,
    );
    const [cards, table] = html.split("<table");
    for (const layout of [cards, table]) {
      expect(layout.match(/Paused accounts/g)).toHaveLength(1);
      expect(layout.match(/Banned accounts/g)).toHaveLength(1);
      for (const account of [...otherStates, mockAccount]) {
        expect(layout).toContain(account.id);
        expect(layout.lastIndexOf(account.id)).toBeLessThan(layout.indexOf("Paused accounts"));
      }
      expect(layout.indexOf("Paused accounts")).toBeLessThan(layout.indexOf(paused.id));
      expect(layout.lastIndexOf(paused.id)).toBeLessThan(layout.indexOf("Banned accounts"));
      expect(layout.indexOf("Banned accounts")).toBeLessThan(layout.indexOf(banned.id));
      expect(layout.lastIndexOf(banned.id)).toBeLessThan(layout.indexOf(secondBanned.id));
    }
    expect(cards.match(/<article /g)).toHaveLength(10);
    expect(table.match(/<tr /g)).toHaveLength(13); // Header, two dividers, ten accounts.
  });

  it("shows the section even when every account is banned, keeping snapshots and actions", () => {
    const html = renderToString(
      <AccountsPanel accounts={[banned]} isLoading={false} onToggleAccount={() => undefined} />,
    );
    const [cards, table] = html.split("<table");
    for (const layout of [cards, table]) {
      expect(layout).toContain("Banned accounts");
      expect(layout).not.toContain("Paused accounts");
      expect(layout).not.toContain("No accounts registered");
      expect(layout).toContain("last known details are kept");
      expect(layout).toContain("banned@example.com");
      expect(layout).toContain("Kiro Pro");
      expect(layout).toContain("37.00%");
      expect(layout).toContain(">73<");
      expect(layout).toContain(">5<");
      expect(layout).toContain("contact support");
    }
    expect(cards).toContain(">Pause<");
    expect(cards).toContain("Delete");
    expect(table).toContain('aria-label="Delete account acc_banned_first"');
  });

  it("does not create a banned section for other exclusions or paused accounts", () => {
    const html = renderToString(
      <AccountsPanel accounts={[paused, ...otherStates]} isLoading={false} />,
    );
    expect(html).toContain("Paused accounts");
    expect(html).not.toContain("Banned accounts");
  });
});

describe("RoutingStateCell", () => {
  const spentAccount: Account = {
    ...mockAccount,
    id: "acc_spent00001",
    routingState: "quota_depleted",
    eligibleInSeconds: 7200,
    quotaHeadroom: 0,
    quotaOverageEnabled: false,
  };

  const exhaustedAccount: Account = {
    ...mockAccount,
    id: "acc_exhausted1",
    routingState: "quota_exhausted",
    eligibleInSeconds: 7200,
  };

  it("labels a spent allowance instead of showing it as ready", () => {
    const html = renderToString(<AccountsPanel accounts={[spentAccount]} isLoading={false} />);
    expect(html).toContain("Quota Spent");
    expect(html).not.toContain("Ready");
  });

  it("does not advertise a spent account as still being tried", () => {
    const html = renderToString(<AccountsPanel accounts={[spentAccount]} isLoading={false} />);
    expect(html).not.toContain("still tried");
  });

  it("reports the reset countdown when one is known", () => {
    const html = renderToString(<AccountsPanel accounts={[spentAccount]} isLoading={false} />);
    expect(html).toContain("resets in");
  });

  it("says so plainly when no reset date is known", () => {
    const html = renderToString(
      <AccountsPanel accounts={[{ ...spentAccount, eligibleInSeconds: 0 }]} isLoading={false} />
    );
    expect(html).toContain("until it resets");
    expect(html).not.toContain("resets in");
  });

  it("renders both quota states the same way, since both exclude", () => {
    // Same evidence-independent outcome, so neither should look milder than the
    // other to whoever is reading the table.
    const spent = renderToString(<AccountsPanel accounts={[spentAccount]} isLoading={false} />);
    const exhausted = renderToString(<AccountsPanel accounts={[exhaustedAccount]} isLoading={false} />);

    // The id appears several times per row (tooltip, aria-label, visible text),
    // so the normalization has to rewrite every occurrence, not the first.
    expect(spent.replaceAll("Quota Spent", "QUOTA").replaceAll(spentAccount.id, "ID")).toBe(
      exhausted.replaceAll("Quota Exhausted", "QUOTA").replaceAll(exhaustedAccount.id, "ID")
    );
  });

  const authDeadAccount: Account = {
    ...mockAccount,
    id: "acc_authdead01",
    routingState: "auth_dead",
  };

  it("labels a rejected credential rather than reporting it as ready", () => {
    const html = renderToString(<AccountsPanel accounts={[authDeadAccount]} isLoading={false} />);
    expect(html).toContain("AUTH DEAD");
    expect(html).not.toContain(">ready<");
  });

  it("names the remedy, because this exclusion is the operator's to fix", () => {
    // A suspension needs Kiro support; a dead credential needs a re-login. The
    // row has to say which, or an operator files the wrong ticket.
    const html = renderToString(<AccountsPanel accounts={[authDeadAccount]} isLoading={false} />);
    expect(html).toContain("re-login required");
    expect(html).not.toContain("contact support");
  });
});

describe("UsageCell error rendering", () => {
  // The exact string that broke the table: httpx's 401 message, 188 characters
  // with an embedded newline.
  const HTTPX_401 =
    "Client error '401 Unauthorized' for url 'https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken'\n" +
    "For more information check: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/401";

  const erroredAccount: Account = {
    ...mockAccount,
    id: "acc_errored001",
    usage: { error: HTTPX_401 },
  };

  it("still reports the error text", () => {
    const html = renderToString(<AccountsPanel accounts={[erroredAccount]} isLoading={false} />);
    expect(html).toContain("401 Unauthorized");
  });

  it("constrains the cell so a long error cannot widen the table", () => {
    // The regression was a nowrap cell growing to fit an unbounded string. The
    // width cap plus wrapping is what keeps the remaining columns on screen.
    const html = renderToString(<AccountsPanel accounts={[erroredAccount]} isLoading={false} />);
    expect(html).toContain("max-w-40");
    expect(html).toContain("whitespace-normal");
    expect(html).toContain("break-words");
    expect(html).toContain("line-clamp-2");
  });

  it("keeps the full message reachable instead of truncating it away", () => {
    const html = renderToString(<AccountsPanel accounts={[erroredAccount]} isLoading={false} />);
    expect(html).toContain("title=");
    expect(html).toContain("developer.mozilla.org");
  });

  it("renders the usage bar for a healthy account, not the error path", () => {
    const healthy: Account = {
      ...mockAccount,
      usage: { usagePercent: 42, currentUsage: 420, usageLimit: 1000, error: null },
    };
    const html = renderToString(<AccountsPanel accounts={[healthy]} isLoading={false} />);
    expect(html).toContain("42.00%");
    expect(html).not.toContain("line-clamp-2");
  });
});
