import { useCallback, useEffect, useState } from "react";
import { ListChecks, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { dashboardApi } from "../api";
import { pushError } from "../alerts";
import { compareModels } from "../model-family";
import { usePreferences } from "../preferences";
import type { DashboardModel } from "../types";
import { ModelMark } from "./model-marks";

export function ModelListingCard({ onNotice }: { onNotice: (message: string) => void }) {
  const { t } = usePreferences();
  const [models, setModels] = useState<DashboardModel[]>([]);
  const [hidden, setHidden] = useState<string[]>([]);
  const [busy, setBusy] = useState<"save" | "refresh" | null>(null);

  const load = useCallback(async () => {
    try {
      const data = await dashboardApi.dashboardModels();
      setModels([...data.models].sort((a, b) => compareModels(a.id, b.id)));
      setHidden(data.hidden ?? []);
    } catch (e) {
      pushError(e);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (id: string, listed: boolean) => {
    const next = listed ? hidden.filter((h) => h !== id) : [...hidden, id];
    setBusy("save");
    try {
      const saved = await dashboardApi.saveListedModels(next);
      setHidden(saved.hidden);
      setModels((current) => current.map((m) => (m.id === id ? { ...m, listed } : m)));
    } catch (e) {
      pushError(e);
    } finally {
      setBusy(null);
    }
  };

  const refresh = async () => {
    setBusy("refresh");
    try {
      const result = await dashboardApi.refreshModels();
      onNotice(t("settings.modelsRefreshed", { accounts: result.refreshed, models: result.models }));
      await load();
    } catch (e) {
      pushError(e);
    } finally {
      setBusy(null);
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <ListChecks size={16} aria-hidden /> {t("settings.listingTitle")}
        </CardTitle>
        <CardDescription>{t("settings.listingDescription")}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          {models.map((m) => {
            const listed = m.listed ?? !hidden.includes(m.id);
            return (
              <label key={m.id} className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={listed}
                  disabled={busy !== null}
                  onChange={(event) => void toggle(m.id, event.target.checked)}
                />
                <ModelMark model={m.id} />
                <span className="font-mono text-xs">{m.listedAs ?? m.id}</span>
              </label>
            );
          })}
        </div>
        <Button variant="secondary" onClick={() => void refresh()} disabled={busy !== null}>
          <RefreshCw size={14} aria-hidden className={busy === "refresh" ? "animate-spin" : undefined} />
          {t("settings.refreshModels")}
        </Button>
      </CardContent>
    </Card>
  );
}
