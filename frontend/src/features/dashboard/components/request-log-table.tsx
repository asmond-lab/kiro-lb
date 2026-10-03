import { useEffect, useState, type ReactNode } from "react";
import { Eye, ScrollText, TriangleAlert } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { EmptyState } from "@/components/empty-state";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { dashboardApi } from "../api";
import { pushError } from "../alerts";
import {
  formatCredits,
  formatLatency,
  formatMultiplier,
  formatRelativeTime,
  formatTimestamp,
} from "../format";
import type { RequestLogDetail, RequestLogOrder, RequestLogPage } from "../types";
import { PaginationControls } from "./pagination-controls";
import { TableSkeleton } from "./skeletons";
import { ModelMark } from "./model-marks";
import { usePreferences } from "../preferences";

export type RequestLogTableProps = {
  page: RequestLogPage;
  isLoading: boolean;
  model: string;
  order: RequestLogOrder;
  onLimitChange: (limit: number) => void;
  onOffsetChange: (offset: number) => void;
  onModelChange: (model: string) => void;
  onOrderChange: (order: RequestLogOrder) => void;
};

const ALL_MODELS = "__all__";

export function RequestLogTable({
  page,
  isLoading,
  model,
  order,
  onLimitChange,
  onOffsetChange,
  onModelChange,
  onOrderChange,
}: RequestLogTableProps) {
  const { t } = usePreferences();
  const isEmpty = !isLoading && page.total === 0;
  const [detail, setDetail] = useState<RequestLogDetail | null>(null);
  const [loadingDetail, setLoadingDetail] = useState<number | null>(null);
  // One shared "now" for every row, ticked coarsely so a long-lived tab does
  // not freeze at "just now"; the lazy initializer keeps the impure Date.now()
  // out of the render body.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  const openDetail = async (id: number) => {
    setLoadingDetail(id);
    try {
      setDetail(await dashboardApi.requestLogDetail(id));
    } catch (error) {
      pushError(error);
    } finally {
      setLoadingDetail(null);
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <ScrollText size={16} aria-hidden /> {t("logs.title")}
        </CardTitle>
        <CardDescription>{t("logs.short")}</CardDescription>
        <CardAction className="flex flex-wrap items-center justify-end gap-2">
          <Select value={model || ALL_MODELS} onValueChange={(value) => onModelChange(value === ALL_MODELS ? "" : value)}>
            <SelectTrigger className="w-56" aria-label={t("logs.filterModel")}>
              <SelectValue placeholder={t("logs.allModels")} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL_MODELS}>{t("logs.allModels")}</SelectItem>
              {(page.models ?? []).map((name) => (
                <SelectItem key={name} value={name}>
                  <ModelMark model={name} />
                  {name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Select value={order} onValueChange={(value) => onOrderChange(value as RequestLogOrder)}>
            <SelectTrigger className="w-44" aria-label={t("logs.sortOrder")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="newest">{t("logs.newest")}</SelectItem>
              <SelectItem value="oldest">{t("logs.oldest")}</SelectItem>
            </SelectContent>
          </Select>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-4">

        {isLoading ? (
          <TableSkeleton rows={Math.min(page.limit, 5)} columns={6} />
        ) : isEmpty ? (
          <EmptyState icon={ScrollText} title={t("logs.emptyTitle")} description={t("logs.emptyDesc")} />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t("logs.time")}</TableHead>
                <TableHead>{t("logs.route")}</TableHead>
                <TableHead>{t("logs.model")}</TableHead>
                <TableHead>{t("logs.status")}</TableHead>
                <TableHead className="hidden text-right md:table-cell">{t("logs.latency")}</TableHead>
                <TableHead className="w-10" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {page.logs.map((log, index) => {
                return (
                  <TableRow key={log.id ?? `${log.created_at}-${page.offset + index}`}>
                    <TableCell title={formatTimestamp(log.created_at)}>
                      {formatRelativeTime(now, log.created_at, t)}
                    </TableCell>
                    <TableCell className="max-w-[10rem] truncate font-mono text-xs md:max-w-none">{log.route}</TableCell>
                    <TableCell>
                      {log.model ? (
                        <span className="inline-flex items-center gap-2 align-middle">
                          <ModelMark model={log.model} />
                          {log.model}
                        </span>
                      ) : (
                        "—"
                      )}
                    </TableCell>
                    <TableCell>
                      {/* Status is the point of the table, so both states must read at a
                        glance: a tinted outline for success against the loud destructive pill. */}
                      <Badge
                        variant={log.status_code < 400 ? "outline" : "destructive"}
                        className={log.status_code < 400 ? "border-success/40 text-success" : undefined}
                      >
                        {log.status_code}
                      </Badge>
                      {log.upstream_cut ? (
                        <span className="ml-2 inline-flex align-middle text-warning" title={t("logs.upstreamCutTitle", { what: log.upstream_cut })}>
                          <TriangleAlert size={14} aria-label={t("logs.upstreamCutTitle", { what: log.upstream_cut })} />
                        </span>
                      ) : null}
                    </TableCell>
                    <TableCell className="hidden text-right tabular-nums md:table-cell">
                      {formatLatency(log.latency_ms)}
                    </TableCell>
                    <TableCell className="text-right">
                      {log.id ? (
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-7"
                          aria-label={t("logs.showDetails")}
                          disabled={loadingDetail !== null}
                          onClick={() => void openDetail(log.id as number)}
                        >
                          <Eye size={14} aria-hidden />
                        </Button>
                      ) : null}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </CardContent>
      {isEmpty ? null : (
        <CardFooter className="border-t">
          <PaginationControls
            total={page.total}
            limit={page.limit}
            offset={page.offset}
            hasMore={page.hasMore}
            onLimitChange={onLimitChange}
            onOffsetChange={onOffsetChange}
          />
        </CardFooter>
      )}
      <RequestDetailDialog detail={detail} onClose={() => setDetail(null)} />
    </Card>
  );
}

function Field({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="space-y-1">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="text-sm break-all">{value ?? "—"}</p>
    </div>
  );
}

export function RequestLogDetailFields({ detail }: { detail: RequestLogDetail }) {
  const { t } = usePreferences();
  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-4 sm:grid-cols-3">
        <Field
          label={t("logs.model")}
          value={
            detail.model ? (
              <span className="inline-flex items-center gap-2">
                <ModelMark model={detail.model} />
                {detail.model}
              </span>
            ) : null
          }
        />
        <Field label={t("logs.status")} value={detail.statusCode} />
        <Field label={t("logs.latency")} value={formatLatency(detail.latencyMs)} />
        <Field label={t("logs.client")} value={detail.clientIp} />
        <Field label={t("logs.userAgent")} value={detail.userAgent} />
        <Field
          label={t("logs.tokensInOut")}
          value={
            detail.inputTokens !== null || detail.outputTokens !== null
              ? `${(detail.inputTokens ?? 0).toLocaleString()} / ${(detail.outputTokens ?? 0).toLocaleString()}`
              : "—"
          }
        />
        <Field
          label={t("logs.tokensPerSecond")}
          value={
            detail.tokensPerSecond != null
              ? `${detail.tokensPerSecond.toFixed(1)} tok/s`
              : "—"
          }
        />
        <Field label={t("logs.ttft")} value={detail.ttftMs != null ? formatLatency(detail.ttftMs) : "—"} />
        <Field label={t("logs.effort")} value={detail.effort ?? "—"} />
        {detail.upstreamCut ? <Field label={t("logs.upstreamCut")} value={detail.upstreamCut} /> : null}
        {detail.creditsSpent != null ? (
          <Field label={t("logs.creditsSpent")} value={formatCredits(detail.creditsSpent)} />
        ) : null}
        {detail.modelMultiplier != null ? (
          <Field label={t("logs.modelMultiplier")} value={formatMultiplier(detail.modelMultiplier)} />
        ) : null}
      </div>
    </div>
  );
}

function RequestDetailDialog({ detail, onClose }: { detail: RequestLogDetail | null; onClose: () => void }) {
  const { t } = usePreferences();
  return (
    <Dialog open={detail !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[85vh] overflow-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{t("logs.detailTitle")}</DialogTitle>
          <DialogDescription>
            {detail ? `${detail.route} · ${formatTimestamp(detail.createdAt)}` : ""}
          </DialogDescription>
        </DialogHeader>
        {detail && <RequestLogDetailFields detail={detail} />}
      </DialogContent>
    </Dialog>
  );
}
