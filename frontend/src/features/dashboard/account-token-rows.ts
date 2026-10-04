import type { AccountRateSeries, AccountTokenUsage, RequestRate } from "./types";

export type AccountTokenRow = {
  account: string;
  email: string | null;
  totalTokens: number;
  promptTokens: number;
  completionTokens: number;
  requests: number;
  models: { model: string; totalTokens: number; requests: number }[];
  rate?: AccountRateSeries;
};

/** Join by the hashed label, never by email or the order of either response. */
export function accountTokenRows(usage: AccountTokenUsage, rate?: RequestRate): AccountTokenRow[] {
  const rates = new Map(rate?.accounts.map((series) => [series.account, series]));
  const accounts = new Set([...Object.keys(usage), ...rates.keys()]);
  return [...accounts].map((account) => {
    const entry = usage[account];
    const models = entry?.models ?? [];
    return {
      account,
      email: entry?.email ?? null,
      totalTokens: entry?.totalTokens ?? 0,
      promptTokens: models.reduce((sum, model) => sum + model.promptTokens, 0),
      completionTokens: models.reduce((sum, model) => sum + model.completionTokens, 0),
      requests: entry?.requests ?? 0,
      models: [...models].sort((a, b) => b.totalTokens - a.totalTokens),
      rate: rates.get(account),
    };
  }).sort((a, b) => b.totalTokens - a.totalTokens);
}
