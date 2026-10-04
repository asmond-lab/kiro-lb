import { useMemo, useState } from "react";
import { ChevronRight, Clock3, Coins, EyeOff } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { EmptyState } from "@/components/empty-state";
import { cn } from "@/lib/utils";
import { ChartSkeleton } from "./skeletons";
import { AccountRateChart, AccountRateDetails } from "./account-rate-chart";
import { accountTokenRows, type AccountTokenRow } from "../account-token-rows";
import { formatClock } from "../dither-series";
import { exactTokens, formatTokens, shareOf } from "../format";
import { isUnroutable } from "../routing-state";
import type { AccountTokenUsage, RequestRate } from "../types";
import { usePreferences } from "../preferences";

/** Rows collapse to a model breakdown on click; more than this and the table dominates the card. */
const VISIBLE_ROWS = 8;
const ROW_COLUMNS = "grid grid-cols-[minmax(0,1fr)_5.5rem] gap-x-5 @2xl/panel:grid-cols-[minmax(0,1fr)_7rem_minmax(16rem,0.9fr)]";

function AccountRow({ row, total }: { row: AccountTokenRow; total: number }) {
  const { t } = usePreferences();
  const [expanded, setExpanded] = useState(false);
  const [showUnroutableRate, setShowUnroutableRate] = useState(false);
  const share = shareOf(row.totalTokens, total);
  const unroutable = row.rate && isUnroutable(row.rate.routingState);

  return (
    <li className={cn(ROW_COLUMNS, "items-center gap-y-3 border-b border-border/50 px-2 py-3 last:border-0")}>
      <button
        type="button"
        onClick={() => setExpanded((open) => !open)}
        aria-expanded={expanded}
        className="group col-span-2 grid min-w-0 grid-cols-subgrid items-center rounded-sm text-left text-sm outline-offset-4 focus-visible:outline-2 focus-visible:outline-ring"
      >
        <span className="flex min-w-0 items-center gap-2.5">
          <ChevronRight size={13} aria-hidden className={cn("shrink-0 text-muted-foreground/60 transition-transform group-hover:text-foreground", expanded && "rotate-90")} />
          <span className="flex min-w-0 flex-col gap-1">
            {/* Keep the label: accounts with the same email must remain distinguishable. */}
            <span className={cn("truncate group-hover:text-foreground", !row.email && "font-mono text-xs")} title={row.email ?? row.account}>
              {row.email ?? row.account}
            </span>
            <span className="flex min-w-0 items-center gap-2 text-[11px] text-muted-foreground">
              <span className={cn("truncate", row.email && "font-mono")} title={row.account}>{row.email ? row.account : t("tokens.keyOnly")}</span>
              {unroutable && <span className="shrink-0 border-l border-border pl-2">{t(`accounts.state.${row.rate?.routingState}`)}</span>}
            </span>
          </span>
        </span>
        <span className="flex flex-col gap-1 text-right tabular-nums">
          <span className="font-medium" title={exactTokens(row.totalTokens)}>{formatTokens(row.totalTokens)}</span>
          <span className="text-[11px] text-muted-foreground">
            {row.totalTokens === 0 ? "0%" : share >= 0.1 ? `${share.toFixed(1)}%` : "<0.1%"}
          </span>
        </span>
      </button>

      <div className="col-span-2 min-w-0 pl-6 @2xl/panel:col-span-1 @2xl/panel:pl-3">
        {row.rate && (!unroutable || showUnroutableRate) ? (
          <div className="flex items-center gap-2">
            <div className="min-w-0 flex-1"><AccountRateChart series={row.rate} /></div>
            {unroutable && (
              <button
                type="button"
                onClick={() => setShowUnroutableRate(false)}
                aria-expanded={showUnroutableRate}
                aria-label={t("tokens.hideRequestHistory")}
                title={t("tokens.hideRequestHistory")}
                className="rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
              >
                <EyeOff size={14} aria-hidden />
              </button>
            )}
          </div>
        ) : (
          <div className="flex h-8 items-center gap-4 text-xs text-muted-foreground">
            <span className="min-w-4 flex-1 border-t border-dashed border-border/60" aria-hidden />
            {unroutable ? (
              <button
                type="button"
                onClick={() => setShowUnroutableRate(true)}
                aria-expanded={showUnroutableRate}
                className="shrink-0 underline decoration-border underline-offset-4 hover:text-foreground"
              >
                {t("tokens.showRequestHistory")}
              </button>
            ) : <span>{t("tokens.noRequestHistory")}</span>}
          </div>
        )}
      </div>

      {expanded && (
        <div className="col-span-full grid gap-4 rounded-md bg-muted/30 p-4 @2xl/panel:grid-cols-2">
          <div className="min-w-0 space-y-2">
            <p className="text-xs font-medium">{t("tokens.byModel")}</p>
            {row.models.length > 0 ? (
              <ul className="space-y-1.5">
                {row.models.map((model) => (
                  <li key={model.model} className="flex items-center gap-3 text-xs text-muted-foreground">
                    <span className="min-w-0 flex-1 truncate font-mono" title={model.model}>{model.model}</span>
                    <span className="shrink-0 tabular-nums" title={exactTokens(model.totalTokens)}>{formatTokens(model.totalTokens)}</span>
                    <span className="shrink-0 text-right tabular-nums">{model.requests.toLocaleString()} {t("tokens.req")}</span>
                  </li>
                ))}
              </ul>
            ) : <p className="text-xs text-muted-foreground">{t("tokens.emptyTitle")}</p>}
          </div>
          {row.rate && (
            <div className="min-w-0 space-y-2 border-t border-border/60 pt-3 @2xl/panel:border-t-0 @2xl/panel:border-l @2xl/panel:pt-0 @2xl/panel:pl-4">
              <p className="text-xs font-medium">{t("tokens.requestRate")}</p>
              <AccountRateDetails series={row.rate} />
            </div>
          )}
        </div>
      )}
    </li>
  );
}

export function AccountTokenPanel({
  accountTokenUsage,
  rate,
  isLoading,
}: {
  accountTokenUsage: AccountTokenUsage;
  rate?: RequestRate;
  isLoading: boolean;
}) {
  const { t } = usePreferences();
  const rows = useMemo(() => accountTokenRows(accountTokenUsage, rate), [accountTokenUsage, rate]);
  const total = useMemo(() => rows.reduce((sum, row) => sum + row.totalTokens, 0), [rows]);
  const [showAll, setShowAll] = useState(false);

  const visible = showAll ? rows : rows.slice(0, VISIBLE_ROWS);
  const inputShare = shareOf(
    rows.reduce((sum, row) => sum + row.promptTokens, 0),
    total,
  );

  return (
    <Card className="@container/panel flex flex-col gap-4">
      <CardHeader className="flex flex-col gap-3 @3xl/panel:flex-row @3xl/panel:items-center @3xl/panel:justify-between">
        <div className="space-y-2">
          <CardTitle className="flex items-center gap-2">
            <Coins size={16} aria-hidden /> {t("tokens.byAccount")}
          </CardTitle>
          <CardDescription>{t("tokens.byAccountDescription")}</CardDescription>
        </div>
        {rate && (
          <p className="flex items-center gap-1.5 text-xs tabular-nums text-muted-foreground" title={t("rate.perAccountDesc")}>
            <Clock3 size={12} aria-hidden className="shrink-0" />
            {rate.bucketStarts.length > 0
              ? t("tokens.rateWindow", {
                  s: rate.bucketSeconds,
                  from: formatClock(rate.bucketStarts[0]),
                  to: formatClock(rate.bucketStarts[rate.bucketStarts.length - 1] + rate.bucketSeconds),
                })
              : t("rate.perAccountDesc")}
          </p>
        )}
      </CardHeader>
      <CardContent className="flex-1">
        {isLoading ? (
          <ChartSkeleton rows={1} />
        ) : rows.length === 0 ? (
          <EmptyState
            icon={Coins}
            title={t("tokens.emptyTitle")}
            description={t("tokens.emptyAccountDescription")}
          />
        ) : (
          <div className="space-y-4">
            <div>
              <div className={cn(ROW_COLUMNS, "border-b px-2 pb-2 text-[11px] text-muted-foreground")} aria-hidden>
                <span className="pl-6">{t("accounts.col.account")}</span>
                <span className="text-right">{t("keys.col.tokens")}</span>
                <span className="hidden pl-3 @2xl/panel:block">{t("tokens.requestRate")}</span>
              </div>
              <ul>
                {visible.map((row) => (
                  <AccountRow key={row.account} row={row} total={total} />
                ))}
              </ul>
            </div>
            {rows.length > VISIBLE_ROWS && (
              <button
                type="button"
                onClick={() => setShowAll((open) => !open)}
                aria-expanded={showAll}
                className="text-xs text-muted-foreground hover:text-foreground"
              >
                {showAll ? t("tokens.showFewer") : t("tokens.showAll", { n: rows.length })}
              </button>
            )}

            <dl className="grid grid-cols-2 gap-4 border-t px-2 pt-4 text-sm @2xl/panel:grid-cols-4">
              <div>
                <dt className="text-xs text-muted-foreground">{t("tokens.input")}</dt>
                <dd className="tabular-nums" title={exactTokens(rows.reduce((s, r) => s + r.promptTokens, 0))}>
                  {formatTokens(rows.reduce((s, r) => s + r.promptTokens, 0))}
                  <span className="ml-1 text-xs text-muted-foreground">{inputShare.toFixed(0)}%</span>
                </dd>
              </div>
              <div>
                <dt className="text-xs text-muted-foreground">{t("tokens.output")}</dt>
                <dd className="tabular-nums" title={exactTokens(rows.reduce((s, r) => s + r.completionTokens, 0))}>
                  {formatTokens(rows.reduce((s, r) => s + r.completionTokens, 0))}
                  <span className="ml-1 text-xs text-muted-foreground">{(total > 0 ? 100 - inputShare : 0).toFixed(0)}%</span>
                </dd>
              </div>
              <div>
                <dt className="text-xs text-muted-foreground">{t("tokens.requests")}</dt>
                <dd className="tabular-nums">{rows.reduce((s, r) => s + r.requests, 0).toLocaleString()}</dd>
              </div>
              <div>
                <dt className="text-xs text-muted-foreground">{t("tokens.accountsUsed")}</dt>
                <dd className="tabular-nums">{Object.keys(accountTokenUsage).length}</dd>
              </div>
            </dl>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
