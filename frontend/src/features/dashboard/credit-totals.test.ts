import { describe, expect, it } from "vitest";
import { creditTotals } from "./credit-totals";
import type { Account } from "./types";

const acc = (over: Partial<Account>): Account => ({ routingState: "available", usage: { currentUsage: 10, usageLimit: 100 }, ...over }) as Account;

describe("creditTotals", () => {
  it("counts availability only from accounts that still have credit", () => {
    const t = creditTotals([
      acc({}),
      acc({ usage: { currentUsage: 90, usageLimit: 50 } }),
      acc({ routingState: "quota_exhausted", usage: { currentUsage: 20, usageLimit: 100 } }),
      acc({ enabled: false }),
      acc({ usage: undefined }),
    ]);
    expect(t).toEqual({ used: 120, limit: 100, available: 90, accounts: 1 });
  });
});
