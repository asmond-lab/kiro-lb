import { translate } from "../preferences";
import type { ToolShortenStats } from "../types";

type Translator = (key: string, vars?: Record<string, string | number>) => string;

const OPTIONS = new Set(["weighted", "most_credits", "sticky", "session"]);

const english: Translator = (key, vars) => translate("en-US", key, vars);

export function loadBalancingLabel(option: string, t: Translator = english): string {
  return OPTIONS.has(option) ? t(`settings.lb.${option}`) : option;
}

export function loadBalancingHelp(option: string | undefined, t: Translator = english): string {
  return option && OPTIONS.has(option) ? t(`settings.lbHelp.${option}`) : "";
}

export function describeShortenStats(
  stats: ToolShortenStats | null | undefined,
  t: Translator = english,
): string | null {
  if (!stats || stats.toolsSeen === 0) {
    return null;
  }
  const saved = Math.max(0, stats.bytesBefore - stats.bytesAfter);
  const percent = stats.bytesBefore > 0 ? Math.round((saved / stats.bytesBefore) * 100) : 0;
  return t("settings.shortenStats", {
    shortened: stats.toolsShortened,
    seen: stats.toolsSeen,
    before: (stats.bytesBefore / 1024).toFixed(1),
    after: (stats.bytesAfter / 1024).toFixed(1),
    percent,
  });
}
