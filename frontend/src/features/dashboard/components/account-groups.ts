import type { Account } from "../types";

export function groupAccounts(accounts: readonly Account[]) {
  const activeAccounts = accounts.filter((account) => account.enabled !== false && account.routingState !== "suspended");
  const pausedAccounts = accounts.filter((account) => account.enabled === false && account.routingState !== "suspended");
  const bannedAccounts = accounts.filter((account) => account.routingState === "suspended");

  return {
    activeAccounts,
    pausedAccounts,
    bannedAccounts,
    displayedAccounts: [...activeAccounts, ...pausedAccounts, ...bannedAccounts],
  };
}
