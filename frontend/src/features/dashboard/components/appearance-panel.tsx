import { Check, Moon, Paintbrush, Sun } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { cn } from "@/lib/utils";
import { Flag } from "./flags";
import { LANGUAGES, setPreferences, usePreferences, type Language, type Theme } from "../preferences";

const THEME_OPTIONS: { id: Theme; icon: typeof Sun; bg: string; card: string; line: string; accent: string }[] = [
  { id: "dark", icon: Moon, bg: "bg-black", card: "bg-neutral-900 border-neutral-800", line: "bg-neutral-700", accent: "bg-neutral-200" },
  { id: "light", icon: Sun, bg: "bg-neutral-100", card: "bg-white border-neutral-200", line: "bg-neutral-300", accent: "bg-neutral-800" },
];

function ThemePreview({ option }: { option: (typeof THEME_OPTIONS)[number] }) {
  return (
    <div className={cn("flex h-24 flex-col gap-1.5 rounded-md p-2.5", option.bg)} aria-hidden>
      <div className="flex items-center gap-1">
        <span className={cn("size-2 rounded-full", option.accent)} />
        <span className={cn("h-1.5 w-10 rounded-full", option.line)} />
      </div>
      <div className={cn("flex flex-1 flex-col gap-1.5 rounded border p-2", option.card)}>
        <span className={cn("h-1.5 w-3/4 rounded-full", option.line)} />
        <span className={cn("h-1.5 w-1/2 rounded-full", option.line)} />
        <span className={cn("mt-auto h-2.5 w-8 rounded-sm", option.accent)} />
      </div>
    </div>
  );
}

export function AppearancePanel() {
  const { theme, language, t } = usePreferences();
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Paintbrush className="size-4" aria-hidden />
          {t("appearance")}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="space-y-2">
          <Label>{t("themeLabel")}</Label>
          <div className="grid max-w-md grid-cols-2 gap-3" role="radiogroup" aria-label={t("themeLabel")}>
            {THEME_OPTIONS.map((option) => {
              const selected = theme === option.id;
              const Icon = option.icon;
              return (
                <button
                  key={option.id}
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  onClick={() => setPreferences({ theme: option.id })}
                  className={cn(
                    "rounded-lg border-2 p-1.5 text-left transition-colors focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none",
                    selected ? "border-primary" : "border-border hover:border-muted-foreground/50",
                  )}
                >
                  <ThemePreview option={option} />
                  <span className="flex items-center justify-between px-1 pt-2 pb-0.5 text-sm font-medium">
                    <span className="flex items-center gap-2">
                      <Icon className="size-4" aria-hidden />
                      {t(option.id)}
                    </span>
                    {selected ? <Check className="size-4 text-primary" aria-hidden /> : null}
                  </span>
                </button>
              );
            })}
          </div>
        </div>
        <div className="space-y-2">
          <Label htmlFor="language">{t("languageLabel")}</Label>
          <Select value={language} onValueChange={(value) => setPreferences({ language: value as Language })}>
            <SelectTrigger id="language" className="w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {LANGUAGES.map((l) => (
                <SelectItem key={l.id} value={l.id}>
                  <Flag language={l.id} />
                  {l.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </CardContent>
    </Card>
  );
}
