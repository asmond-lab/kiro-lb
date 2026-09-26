import { Activity, HeartPulse, Server, Users } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
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

export function InfoPanel({ overview, accounts, routableAccounts, lastUpdatedAt, isLive }: InfoPanelProps) {
  const { t } = usePreferences();
  const enabled = accounts.filter((account) => account.enabled !== false);
  const disabled = accounts.length - enabled.length;
  const suspended = enabled.filter((account) => account.routingState === "suspended").length;
  const cooling = enabled.filter((account) => account.routingState === "cooling_down").length;
  const rateLimited = enabled.filter((account) => account.routingState === "rate_limited").length;

  return (
    <div className="grid gap-6 xl:grid-cols-2">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <HeartPulse size={16} aria-hidden /> {t("info.service")}
          </CardTitle>
          <CardDescription>{t("info.serviceDesc")}</CardDescription>
        </CardHeader>
        <CardContent>
          <Table>
            <TableBody>
              <Row
                label={t("info.status")}
                value={
                  <Badge
                    variant={overview?.proxy.status === "healthy" ? "outline" : "destructive"}
                    className={
                      overview?.proxy.status === "healthy" ? "border-success/40 text-success" : undefined
                    }
                  >
                    {overview?.proxy.status ?? t("info.unknown")}
                  </Badge>
                }
              />
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
    </div>
  );
}
