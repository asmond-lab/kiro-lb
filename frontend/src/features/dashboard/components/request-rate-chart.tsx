import { useMemo, useState } from "react";
import { Activity } from "lucide-react";
import { Area } from "@/components/dither-kit/area";
import { AreaChart } from "@/components/dither-kit/area-chart";
import { ReferenceLine } from "@/components/dither-kit/reference-line";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { EmptyState } from "@/components/empty-state";
import { accountRateSeries } from "../dither-series";
import { isUnroutable } from "../routing-state";
import type { AccountRateSeries } from "../types";
import type { RequestRate } from "../types";
import { ChartSkeleton } from "./skeletons";
import { usePreferences } from "../preferences";

function formatClock(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}



function AccountRatePanel({ series }: { series: AccountRateSeries }) {
  const { t } = usePreferences();
  const view = useMemo(() => accountRateSeries(series), [series]);

  return (
    <div className="space-y-2 rounded-lg border p-3">
      <div className="flex items-baseline justify-between gap-2">
        <span className="flex min-w-0 items-baseline gap-1.5">
          <span className="font-mono text-xs">{series.account}</span>
          {/* Only shown when the panel was revealed by the toggle. Without it a
              suspended account is indistinguishable from an idle healthy one. */}
          {isUnroutable(series.routingState) && (
            <Badge variant="destructive" className="text-[10px]">
              {series.routingState === "suspended" ? t("rate.banned") : t("rate.noQuota")}
            </Badge>
          )}
        </span>
        <span className="text-xs tabular-nums text-muted-foreground">
          {t("rate.peakPerMin", { n: view.peak })}
          {view.load !== null && (
            <span className={view.nearLimit ? "text-destructive" : undefined}>
              {" "}
              · {t("rate.ofLimit", { n: Math.round(view.load * 100) })}
            </span>
          )}
          {view.rejected > 0 && <span className="text-destructive"> · {t("rate.nRejected", { n: view.rejected })}</span>}
        </span>
      </div>

      {/* An idle account keeps its panel: the grid must not change shape with
          traffic, or an account that served nothing looks like one that is not
          in the pool at all. */}
      {!view.hasTraffic ? (
        <div className="flex h-28 items-center justify-center rounded-md border border-dashed border-border/60">
          <p className="text-xs text-muted-foreground">{t("rate.noTraffic")}</p>
        </div>
      ) : (
        <div className="relative">
          <div
            className="h-28 w-full"
            role="img"
            aria-label={`${t("rate.accountAria", { account: series.account, peak: view.peak })}${
              series.limitRpm === null ? t("rate.accountAriaNoLimit") : t("rate.accountAriaLimit", { n: series.limitRpm })
            }`}
          >
            <AreaChart
              data={view.rows}
              config={view.config}
              interactive={false}
              animate={false}
              yMax={view.yMax}
              margins={{ top: 6, right: 2, bottom: 2, left: 2 }}
            >
              <Area dataKey="rpm" variant="gradient" />
              {series.limitRpm !== null && (
                <ReferenceLine y={series.limitRpm} className="stroke-destructive/70" />
              )}
            </AreaChart>
          </div>
          {series.limitRpm !== null && (
            <Badge variant="destructive" className="absolute right-1 top-1 text-[10px]">
              ~{series.limitRpm}/min
            </Badge>
          )}
        </div>
      )}

      <p className="text-xs text-muted-foreground">
        {series.limitRpm === null ? (
          <>
            {t("rate.noGuide", { reason: series.limitUnknownReason ?? "" })}
            {series.safeRpm > 0 && t("rate.servedSafe", { n: series.safeRpm })}
          </>
        ) : view.nearLimit ? (
          <span className="text-destructive">
            {t("rate.approaching", { n: series.limitRpm })}
          </span>
        ) : (
          <>
            {t(series.informativeSamples === 1 ? "rate.limitBetweenOne" : "rate.limitBetweenMany", {
              safe: series.safeRpm,
              limit: series.limitRpm ?? "",
              precision: series.limitPrecisionRpm ?? "",
              n: series.informativeSamples,
              h: Math.round(series.estimateWindowSeconds / 3600),
            })}
          </>
        )}
      </p>
    </div>
  );
}

export function RequestRateChart({ rate, isLoading }: { rate?: RequestRate; isLoading: boolean }) {
  const { t } = usePreferences();
  const [showUnroutable, setShowUnroutable] = useState(false);

  const { shown, hidden } = useMemo(() => {
    const all = rate?.accounts ?? [];
    return {
      shown: all.filter((series) => !isUnroutable(series.routingState)),
      hidden: all.filter((series) => isUnroutable(series.routingState)),
    };
  }, [rate]);

  // Emptiness is a per-account fact, so it is reported inside each panel. The
  // card only collapses when there is no account to chart at all.
  const hasAccounts = (rate?.accounts.length ?? 0) > 0;
  const visible = showUnroutable ? [...shown, ...hidden] : shown;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("rate.perAccountTitle")}</CardTitle>
        <CardDescription>
          {rate
            ? t("rate.perAccountDescWindow", {
                s: rate.bucketSeconds,
                from: formatClock(rate.bucketStarts[0]),
                to: formatClock(rate.bucketStarts[rate.bucketStarts.length - 1] + rate.bucketSeconds),
              })
            : t("rate.perAccountDesc")}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading || !rate ? (
          <ChartSkeleton />
        ) : !hasAccounts ? (
          <EmptyState
            icon={Activity}
            title={t("rate.noAccounts")}
            description={t("rate.noAccountsDesc")}
          />
        ) : (
          <div className="space-y-3">
            {visible.length > 0 ? (
              <div className="grid gap-3 lg:grid-cols-2">
                {visible.map((series) => (
                  <AccountRatePanel key={series.account} series={series} />
                ))}
              </div>
            ) : (
              // Every account in the pool is unroutable. Saying so beats an
              // empty card that reads as "no data".
              <EmptyState
                icon={Activity}
                title={t("rate.noRoutable")}
                description={t("rate.noRoutableDesc")}
              />
            )}

            {/* Hidden panels are disclosed, never silently dropped: an operator
                who knows the pool size would otherwise be left wondering which
                account went missing and why. */}
            {hidden.length > 0 && (
              <button
                type="button"
                onClick={() => setShowUnroutable((previous) => !previous)}
                aria-expanded={showUnroutable}
                className="text-xs text-muted-foreground underline-offset-2 hover:underline"
              >
                {showUnroutable
                  ? t(hidden.length === 1 ? "rate.hideOne" : "rate.hideMany", { n: hidden.length })
                  : t(hidden.length === 1 ? "rate.hiddenOne" : "rate.hiddenMany", { n: hidden.length })}
              </button>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
