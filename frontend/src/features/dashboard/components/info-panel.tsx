import { Activity, ExternalLink, HeartPulse, RefreshCw, Server, Users } from "lucide-react";
import { useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table";
import { formatTimestamp } from "../format";
import type { Account, Overview } from "../types";
import { usePreferences } from "../preferences";

export type InfoPanelProps = {
  overview?: Overview;
  accounts: Account[];
  routableAccounts?: number;
  lastUpdatedAt?: number;
  isLive: boolean;
  isCheckingUpdates: boolean;
  onCheckUpdates: () => void;
  isInstallingUpdate: boolean;
  onInstallUpdate: (version: string) => void;
};

function uptime(seconds?: number): string {
  if (!seconds) return "—";
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days) return `${days}d ${hours}h`;
  if (hours) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <TableRow>
      <TableCell className="w-1/2 text-muted-foreground">{label}</TableCell>
      <TableCell className="text-right font-medium">{value}</TableCell>
    </TableRow>
  );
}

export function InfoPanel({ overview, accounts, routableAccounts, lastUpdatedAt, isLive, isCheckingUpdates, onCheckUpdates, isInstallingUpdate, onInstallUpdate }: InfoPanelProps) {
  const { t } = usePreferences();
  const [confirmVersion, setConfirmVersion] = useState<string | null>(null);
  const enabled = accounts.filter((account) => account.enabled !== false);
  const disabled = accounts.length - enabled.length;
  const suspended = enabled.filter((account) => account.routingState === "suspended").length;
  const cooling = enabled.filter((account) => account.routingState === "cooling_down").length;
  const rateLimited = enabled.filter((account) => account.routingState === "rate_limited").length;
  const version = overview?.version;
  const update = overview?.update;
  const versionStatus = isCheckingUpdates ? "checking" : version?.status ?? (overview ? "unavailable" : "checking");

  return (
    <div className="grid gap-6 xl:grid-cols-2">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <HeartPulse size={16} aria-hidden /> {t("info.service")}
          </CardTitle>
          <CardDescription>{t("info.serviceDesc")}</CardDescription>
          <CardAction>
            <Badge
              aria-label={`${t("info.status")}: ${overview?.proxy.status ?? t("info.unknown")}`}
              variant={overview?.proxy.status === "healthy" ? "outline" : "destructive"}
              className={overview?.proxy.status === "healthy" ? "border-success/40 text-success" : undefined}
            >
              {overview?.proxy.status ?? t("info.unknown")}
            </Badge>
          </CardAction>
        </CardHeader>
        <CardContent className="space-y-3">
          <section aria-label={t("info.updates")} className="space-y-3 border-b pb-4">
            <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
              <p className="text-xs text-muted-foreground">{t("info.currentVersion")}</p>
              <p className="min-w-0 break-all font-mono text-base font-semibold">
                {version ? `v${version.current}` : "—"}
              </p>
              <Badge
                variant="outline"
                className={`max-w-full whitespace-normal ${
                  versionStatus === "latest" ? "border-success/40 text-success" :
                  versionStatus === "update_available" ? "border-warning/40 text-warning" :
                  "text-muted-foreground"
                }`}
              >
                {t(`info.versionStatus.${versionStatus}`)}
              </Badge>
            </div>
            {version?.latest && version.releaseUrl && (versionStatus === "update_available" || versionStatus === "ahead") && (
              <a
                href={version.releaseUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex max-w-full items-center gap-1.5 text-sm text-primary underline-offset-4 hover:underline"
              >
                <span className="break-all">{t("info.latestRelease", { version: version.latest })}</span>
                <ExternalLink className="size-3.5 shrink-0" aria-hidden />
              </a>
            )}
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={isCheckingUpdates || isInstallingUpdate}
                onClick={onCheckUpdates}
                title={t("info.checkUpdatesHint")}
                aria-busy={isCheckingUpdates}
              >
                <RefreshCw aria-hidden className={isCheckingUpdates ? "animate-spin" : undefined} />
                {t("info.checkUpdates")}
              </Button>
              {versionStatus === "update_available" && version?.latest && update && !update.disabledReason && (
                <Button size="sm" disabled={isCheckingUpdates || isInstallingUpdate} onClick={() => setConfirmVersion(version.latest)}>
                  {t("info.installUpdate")}
                </Button>
              )}
            </div>
            {versionStatus === "update_available" && update?.disabledReason && (
              <p className="text-sm leading-relaxed text-muted-foreground">{t(`info.installDisabled.${update.disabledReason}`)}</p>
            )}
            {isInstallingUpdate && !isCheckingUpdates && (
              <p role="status" className="flex items-start gap-2 rounded-md bg-muted/40 p-3 text-sm text-muted-foreground">
                <RefreshCw className="mt-0.5 size-4 shrink-0 animate-spin" aria-hidden />
                {t(update?.status === "restarting" ? "info.restartingUpdate" : "info.downloadingUpdate")}
              </p>
            )}
            {update?.status === "failed" && (
              <p role="alert" className="break-words rounded-md border border-destructive/20 bg-destructive/5 p-3 text-sm text-destructive">
                {t("info.installFailed")} {update.error}
              </p>
            )}
          </section>
          <Table>
            <TableBody>
              <Row label={t("info.uptime")} value={uptime(overview?.proxy.uptimeSeconds)} />
              <Row
                label={t("info.averageLatency")}
                value={overview ? `${Math.round(overview.averageLatencyMs)} ms` : "—"}
              />
              <Row label={t("info.liveUpdates")} value={isLive ? t("info.on") : t("info.paused")} />
              <Row label={t("info.lastUpdate")} value={formatTimestamp(lastUpdatedAt ? lastUpdatedAt / 1000 : undefined)} />
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Users size={16} aria-hidden /> {t("info.accountPool")}
          </CardTitle>
          <CardDescription>{t("info.accountPoolDesc")}</CardDescription>
        </CardHeader>
        <CardContent>
          <Table>
            <TableBody>
              <Row label={t("info.routableNow")} value={t("info.nOfM", { n: routableAccounts ?? 0, m: enabled.length })} />
              <Row label={t("info.initialized")} value={t("info.nOfM", { n: overview?.accounts.initialized ?? 0, m: overview?.accounts.total ?? 0 })} />
              <Row label={t("info.disabled")} value={disabled} />
              <Row
                label={t("info.suspended")}
                value={suspended ? <span className="text-destructive">{suspended}</span> : 0}
              />
              <Row label={t("info.cooling")} value={cooling} />
              <Row label={t("info.rateLimited")} value={rateLimited} />
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Activity size={16} aria-hidden /> {t("info.traffic")}
          </CardTitle>
          <CardDescription>{t("info.trafficDesc")}</CardDescription>
        </CardHeader>
        <CardContent>
          <Table>
            <TableBody>
              <Row label={t("info.requests")} value={(overview?.requests24h ?? 0).toLocaleString()} />
              <Row label={t("info.successful")} value={(overview?.successes24h ?? 0).toLocaleString()} />
              <Row
                label={t("info.failed")}
                value={Math.max(0, (overview?.requests24h ?? 0) - (overview?.successes24h ?? 0)).toLocaleString()}
              />
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Server size={16} aria-hidden /> {t("info.models")}
          </CardTitle>
          <CardDescription>{t("info.modelsDesc")}</CardDescription>
        </CardHeader>
        <CardContent>
          <Table>
            <TableBody>
              <Row label={t("info.available")} value={overview?.models ?? 0} />
            </TableBody>
          </Table>
        </CardContent>
      </Card>
      <Dialog open={confirmVersion !== null} onOpenChange={(open) => !open && setConfirmVersion(null)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("info.installTitle", { version: confirmVersion ?? "" })}</DialogTitle>
            <DialogDescription>{t("info.installConfirm")}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmVersion(null)}>{t("accounts.cancel")}</Button>
            <Button disabled={isInstallingUpdate} onClick={() => {
              if (confirmVersion) onInstallUpdate(confirmVersion);
              setConfirmVersion(null);
            }}>{t("info.installUpdate")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
