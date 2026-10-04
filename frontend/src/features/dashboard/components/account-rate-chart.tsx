import { useMemo } from "react";
import { Area } from "@/components/dither-kit/area";
import { AreaChart } from "@/components/dither-kit/area-chart";
import { ReferenceLine } from "@/components/dither-kit/reference-line";
import { accountRateSeries } from "../dither-series";
import { rateGuideReason } from "../routing-state";
import type { AccountRateSeries } from "../types";
import { usePreferences } from "../preferences";

export function AccountRateChart({ series }: { series: AccountRateSeries }) {
  const { t } = usePreferences();
  const view = useMemo(() => accountRateSeries(series), [series]);

  return (
    <div className="flex min-w-0 items-center gap-4">
      <div
        className="h-8 min-w-0 flex-1"
        role="img"
        aria-label={`${t("rate.accountAria", { account: series.account, peak: view.peak })}${
          series.limitRpm === null ? t("rate.accountAriaNoLimit") : t("rate.accountAriaLimit", { n: series.limitRpm })
        }`}
      >
        {view.hasTraffic ? (
          <AreaChart
            data={view.rows}
            config={view.config}
            interactive={false}
            animate={false}
            yMax={view.yMax}
            margins={{ top: 4, right: 2, bottom: 2, left: 2 }}
          >
            <Area dataKey="rpm" variant="gradient" />
            {series.limitRpm !== null && (
              <ReferenceLine y={series.limitRpm} className="stroke-destructive/50" />
            )}
          </AreaChart>
        ) : (
          <div className="flex h-full items-center" title={t("rate.noTraffic")}>
            <span className="w-full border-t border-dashed border-border" />
          </div>
        )}
      </div>
      <div className="w-24 shrink-0 text-right text-xs tabular-nums">
        <p className={view.nearLimit ? "text-destructive" : "text-foreground/80"}>
          {t("rate.peakPerMin", { n: view.peak })}
        </p>
        {view.rejected > 0 ? (
          <p className="mt-0.5 text-[11px] text-destructive">{t("rate.nRejected", { n: view.rejected })}</p>
        ) : !view.hasTraffic ? (
          <p className="mt-0.5 text-[11px] text-muted-foreground">{t("tokens.idle")}</p>
        ) : null}
      </div>
    </div>
  );
}

/** The longer guide belongs with the model breakdown, not inside the sparkline. */
export function AccountRateDetails({ series }: { series: AccountRateSeries }) {
  const { t } = usePreferences();
  const view = useMemo(() => accountRateSeries(series), [series]);

  return (
    <div className="space-y-2 text-xs leading-relaxed text-muted-foreground">
      {view.load !== null && (
        <p className={view.nearLimit ? "text-destructive" : undefined}>
          {t("rate.ofLimit", { n: Math.round(view.load * 100) })}
          {view.rejected > 0 && <> · {t("rate.nRejected", { n: view.rejected })}</>}
        </p>
      )}
      <p>
        {series.limitRpm === null ? (
          <>
            {t("rate.noGuide", { reason: rateGuideReason(series.limitUnknownReason, t) })}
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
