import { type ReactNode } from "react";
import { LogOut } from "lucide-react";
import { Button } from "@/components/ui/button";
import { usePreferences } from "../preferences";
import { GHOST_BODY, GHOST_EYES, KIRO_LETTERS, LB_OUTLINES } from "../wordmark-paths";

export function KiroLogo({ size = 36 }: { size?: number }) {
  return <img src="/kiro-icon.svg" width={size} height={size} alt="" className="rounded-lg" />;
}

export function KiroLbWordmark({ height = 28 }: { height?: number }) {
  return (
    <svg height={height} viewBox="0 0 118 26" fill="none" role="img" aria-label="KiroLB">
      <path d={GHOST_BODY} fill="currentColor" />
      {GHOST_EYES.map((d) => (
        <path key={d.slice(0, 16)} d={d} className="fill-background" />
      ))}

      {KIRO_LETTERS.map((d) => (
        <path key={d.slice(0, 16)} d={d} fill="currentColor" />
      ))}
      <g transform="translate(3 0)" fill="none" stroke="currentColor" strokeWidth="0.94" strokeLinejoin="round">
        {LB_OUTLINES.map((d) => (
          <path key={d.slice(0, 16)} d={d} />
        ))}
      </g>
    </svg>
  );
}

export function StatCard({ label, value, icon }: { label: string; value: ReactNode; icon: ReactNode }) {
  return (
    <div className="flex min-w-0 items-center gap-3 bg-card px-4 py-3">
      <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-muted/40 text-muted-foreground [&_svg]:size-4">
        {icon}
      </span>
      <div className="min-w-0">
        <div className="truncate text-xs text-muted-foreground">{label}</div>
        <div className="truncate text-base font-semibold tracking-tight tabular-nums">{value}</div>
      </div>
    </div>
  );
}

export function SignOutButton({ vertical = false, onSignOut }: { vertical?: boolean; onSignOut: () => void }) {
  const { t } = usePreferences();
  return (
    <Button
      variant="outline"
      size="sm"
      className={vertical ? "w-full justify-start gap-3 px-3" : undefined}
      onClick={onSignOut}
    >
      <LogOut />
      {t("signOut")}
    </Button>
  );
}
