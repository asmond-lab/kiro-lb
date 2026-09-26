import { ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { usePreferences } from "../preferences";

const PAGE_SIZE_OPTIONS = [10, 25, 50, 100];

export type PaginationControlsProps = {
  total: number;
  limit: number;
  offset: number;
  hasMore: boolean;
  onLimitChange: (limit: number) => void;
  onOffsetChange: (offset: number) => void;
};

export function PaginationControls({
  total,
  limit,
  offset,
  hasMore,
  onLimitChange,
  onOffsetChange,
}: PaginationControlsProps) {
  const { t } = usePreferences();
  const lastPageOffset = total > 0 ? Math.max(0, Math.ceil(total / limit) - 1) * limit : 0;
  const rangeStart = total > 0 ? offset + 1 : 0;
  const rangeEnd = Math.min(offset + limit, total);

  return (
    <div className="flex flex-wrap items-center justify-end gap-2 text-xs">
      <span className="text-muted-foreground">{t("logs.rows")}</span>
      <Select value={String(limit)} onValueChange={(value) => onLimitChange(Number(value))}>
        <SelectTrigger size="sm" className="w-20" aria-label={t("logs.rowsPerPage")}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent align="end">
          {PAGE_SIZE_OPTIONS.map((size) => (
            <SelectItem key={size} value={String(size)}>
              {size}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      <span className="whitespace-nowrap tabular-nums text-muted-foreground">
        {t("logs.range", { start: rangeStart, end: rangeEnd, total: total.toLocaleString() })}
      </span>

      {/* One wrapper so the four nav buttons wrap as a unit, never mid-group. */}
      <div className="flex shrink-0 items-center gap-2">
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          disabled={offset <= 0}
          onClick={() => onOffsetChange(0)}
          aria-label={t("logs.firstPage")}
        >
          <ChevronsLeft />
        </Button>
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          disabled={offset <= 0}
          onClick={() => onOffsetChange(Math.max(0, offset - limit))}
          aria-label={t("logs.previousPage")}
        >
          <ChevronLeft />
        </Button>
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          disabled={!hasMore}
          onClick={() => onOffsetChange(offset + limit)}
          aria-label={t("logs.nextPage")}
        >
          <ChevronRight />
        </Button>
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          disabled={!hasMore}
          onClick={() => onOffsetChange(lastPageOffset)}
          aria-label={t("logs.lastPage")}
        >
          <ChevronsRight />
        </Button>
      </div>
    </div>
  );
}
