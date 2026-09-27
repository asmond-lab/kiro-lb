import type { Account } from "./types";

export type CreditTotals = { used: number; limit: number; available: number; accounts: number };

const SPENT = new Set(["disabled", "quota_exhausted", "quota_depleted", "suspended", "auth_dead"]);

export function creditTotals(accounts: Account[]): CreditTotals {
  let used = 0;
  let limit = 0;
  let available = 0;
  let counted = 0;
  for (const account of accounts) {
    if (account.enabled === false || account.routingState === "disabled") continue;
    const u = account.usage;
    if (u?.usageLimit == null || u.currentUsage == null) continue;
    used += u.currentUsage;
    const left = u.usageLimit - u.currentUsage;
    if (SPENT.has(account.routingState) || left <= 0) continue;
    limit += u.usageLimit;
    available += left;
    counted += 1;
  }
  return { used, limit, available, accounts: counted };
}
