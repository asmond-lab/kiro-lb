import { useMemo } from "react";
import { Activity } from "lucide-react";
import { Area } from "@/components/dither-kit/area";
import { AreaChart } from "@/components/dither-kit/area-chart";
import { Grid } from "@/components/dither-kit/grid";
import { ReferenceLine } from "@/components/dither-kit/reference-line";
import { Tooltip } from "@/components/dither-kit/tooltip";
import { XAxis } from "@/components/dither-kit/x-axis";
import { YAxis } from "@/components/dither-kit/y-axis";
import { EmptyState } from "@/components/empty-state";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ChartSkeleton } from "./skeletons";
import { rateChartConfig, rateChartRows } from "../dither-series";
import { summarizeRate, throttledAccounts } from "../request-rate-totals";
import { PANEL_UPPER_MIN_HEIGHT } from "../panel-metrics";
import type { RequestRate } from "../types";
import { usePreferences } from "../preferences";

function formatClock(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function round(value: number): string {
  // A rate is rarely a whole number once buckets are scaled to a minute, but a
  // trailing ".0" on every axis label is noise.
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}

function Figure({ label, value, hint, tone }: { label: string; value: string; hint?: string; tone?: "warning" }) {
  return (
    <div>
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className={`tabular-nums ${tone === "warning" ? "text-destructive" : ""}`}>
        {value}
        {hint && <span className="ml-1 text-xs text-muted-foreground">{hint}</span>}
      </dd>
    </div>
  );
}

export function TotalRateChart({ rate, isLoading }: { rate?: RequestRate; isLoading: boolean }) {
  const { t } = usePreferences();
  const totals = useMemo(() => summarizeRate(rate), [rate]);
  const throttled = useMemo(() => throttledAccounts(rate), [rate]);
  const rows = useMemo(() => rateChartRows(totals), [totals]);
  const config = useMemo(() => rateChartConfig(totals), [totals]);

  return (
    <Card className="@container/panel flex flex-col">
      <CardHeader>
        <CardTitle>{t("rate.totalTitle")}</CardTitle>
        <CardDescription>
          {rate
            ? t("rate.totalDescWindow", {
                s: rate.bucketSeconds,
                from: formatClock(rate.bucketStarts[0]),
                to: formatClock(rate.bucketStarts[rate.bucketStarts.length - 1] + rate.bucketSeconds),
              })
            : t("rate.totalDesc")}
        </CardDescription>
      </CardHeader>
      <CardContent className="flex-1">
        {isLoading || !rate ? (
          <ChartSkeleton rows={1} />
        ) : (
          <div className="space-y-4">
            {/* Floor the plot area so this card's divider lines up with the token
                panel's when the two share a row. */}
            <div className={`flex flex-col justify-center ${PANEL_UPPER_MIN_HEIGHT}`}>
              <div
                className="h-44 w-full"
                role="img"
                aria-label={t("rate.totalAria", {
                  peak: round(totals.peakPerMinute),
                  avg: round(totals.meanPerMinute),
                  n: totals.requests,
                })}
              >
                {totals.requests === 0 ? (
                  <EmptyState
                    icon={Activity}
                    title={t("rate.noRequests")}
                    description={t("rate.noRequestsDesc")}
                  />
                ) : (
                  /* No entrance sweep: live polling bumps the data revision every
                     second, so the reveal would replay forever instead of playing once. */
                  <AreaChart data={rows} config={config} bloom="low" bloomOnHover animate={false}>
                    <Grid />
                    <XAxis dataKey="at" maxTicks={6} />
                    <YAxis tickCount={3} tickFormatter={round} />
                    {/* The mean makes a spike legible as a spike rather than as the
                        normal level, which a bare area chart cannot convey. */}
                    <ReferenceLine y={totals.meanPerMinute} label={t("rate.avg", { n: round(totals.meanPerMinute) })} />
                    <Area dataKey="served" variant="gradient" />
                    {/* Rejections are drawn on top: they are rare and must not be
                        lost inside the total they are part of. */}
                    {config.rejected && <Area dataKey="rejected" variant="hatched" />}
                    <Tooltip labelKey="at" valueFormatter={(value) => `${round(value)}/min`} />
                  </AreaChart>
                )}
              </div>
            </div>

            {/* Container queries, not viewport ones: this panel sits full-width on
                its own and half-width beside the token chart, so the column count
                has to follow the card rather than the screen. */}
            <dl className="grid grid-cols-2 gap-3 border-t pt-4 @md/panel:grid-cols-3 @2xl/panel:grid-cols-5">
              <Figure
                label={t("rate.peak")}
                value={`${round(totals.peakPerMinute)}/min`}
                hint={t("rate.burst", { n: totals.peakConcurrentRpm })}
              />
              <Figure label={t("rate.average")} value={`${round(totals.meanPerMinute)}/min`} />
              <Figure label={t("rate.requests")} value={totals.requests.toLocaleString()} hint={t("rate.inWindow")} />
              <Figure
                label={t("rate.rejected")}
                value={totals.rateLimited.toLocaleString()}
                tone={totals.rateLimited > 0 ? "warning" : undefined}
                hint={throttled.length > 0 ? t(throttled.length === 1 ? "rate.accountsOne" : "rate.accountsMany", { n: throttled.length }) : undefined}
              />
              <Figure
                label={t("rate.failed")}
                value={totals.failures.toLocaleString()}
                tone={totals.failures > 0 ? "warning" : undefined}
              />
            </dl>

            {totals.rateLimited > 0 && (
              <p className="text-xs text-destructive">
                {t(totals.rateLimited === 1 ? "rate.rejectionsOne" : "rate.rejectionsMany", {
                  n: totals.rateLimited,
                  accounts: throttled.join(", "),
                })}
              </p>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
