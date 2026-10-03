import { useEffect, useRef } from "react";
import { CircleCheck, TriangleAlert, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { dismissAlert, useAlerts, type AlertItem } from "../alerts";
import { localizeError } from "../error-text";
import { usePreferences } from "../preferences";

const TONE_CLASS: Record<AlertItem["tone"], string> = {
  error: "border-destructive/30 bg-destructive/10 text-destructive",
  warning: "border-warning/30 bg-warning/10 text-warning",
  success: "border-success/30 bg-success/10 text-success",
};

export function AlertStack() {
  const alerts = useAlerts();
  const { t, language } = usePreferences();
  const containerRef = useRef<HTMLDivElement>(null);
  const latestId = alerts.filter((alert) => !alert.leaving).at(-1)?.id;

  useEffect(() => {
    const element = containerRef.current;
    if (latestId === undefined || !element) return;
    const { top, bottom } = element.getBoundingClientRect();
    if (top >= 0 && bottom <= window.innerHeight) return;
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    element.scrollIntoView({ block: "start", behavior: reduce ? "auto" : "smooth" });
  }, [latestId]);

  return (
    <div ref={containerRef} className="scroll-mt-4">
      {alerts.map((alert) => {
        const text = alert.text ?? (alert.key ? t(alert.key, alert.vars) : localizeError(language, alert.error));
        const Icon = alert.tone === "success" ? CircleCheck : TriangleAlert;
        return (
          <div
            key={alert.id}
            className={`grid transition-[grid-template-rows,opacity] duration-500 ease-out motion-reduce:transition-none ${
              alert.leaving ? "grid-rows-[0fr] opacity-0" : "grid-rows-[1fr] opacity-100"
            }`}
          >
            <div className="min-h-0 overflow-hidden">
              <div
                role={alert.tone === "success" ? "status" : "alert"}
                aria-live={alert.tone === "success" ? "polite" : "assertive"}
                className={`border-b ${TONE_CLASS[alert.tone]}`}
              >
                <div className="flex items-center justify-between gap-3 px-4 py-2 text-sm sm:px-6">
                  <span className="flex min-w-0 flex-1 items-center gap-2 break-words [overflow-wrap:anywhere]">
                    <Icon size={15} aria-hidden className="shrink-0" />
                    {text}
                  </span>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-8 shrink-0"
                    aria-label={t(alert.tone === "success" ? "overview.dismissNotice" : "overview.dismissError")}
                    onClick={() => dismissAlert(alert.id)}
                  >
                    <X aria-hidden />
                  </Button>
                </div>
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
