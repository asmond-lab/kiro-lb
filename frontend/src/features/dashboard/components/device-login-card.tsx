import { useCallback, useEffect, useRef, useState } from "react";
import type { ComponentType } from "react";
import { Link2, LogIn, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { dashboardApi } from "../api";
import { dismissAlert, pushAlert, pushError } from "../alerts";
import { registrationMessage } from "../device-login-result";
import type { DeviceLoginFlow, DeviceLoginProvider } from "../types";
import { AwsMark, GithubMark, GoogleMark } from "./provider-marks";
import { usePreferences } from "../preferences";

const POLL_INTERVAL_MS = 2500;

const PROVIDERS: { id: DeviceLoginProvider; label: string; mark: ComponentType<{ size?: number }> }[] = [
  { id: "builder-id", label: "AWS Builder ID", mark: AwsMark },
  { id: "google", label: "Google", mark: GoogleMark },
  { id: "github", label: "GitHub", mark: GithubMark },
];

export function DeviceLoginCard({ onRegistered }: { onRegistered: () => Promise<void> }) {
  const { t, language } = usePreferences();
  const [flow, setFlow] = useState<DeviceLoginFlow>();
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const [deadline, setDeadline] = useState(0);
  const registering = useRef(false);
  const linkAlert = useRef<number | undefined>(undefined);

  const start = async (provider: DeviceLoginProvider) => {
    setBusy(true);
    try {
      const started = await dashboardApi.startDeviceLogin(provider);
      setFlow(started);
      setDeadline(Date.now() + started.expiresInSeconds * 1000);
      setNow(Date.now());
      await copyLink(started.verificationUriComplete);
    } catch (cause) {
      pushError(cause);
    } finally {
      setBusy(false);
    }
  };

  const copyLink = async (url: string) => {
    try {
      await navigator.clipboard.writeText(url);
      linkAlert.current = pushAlert({ tone: "success", key: "accounts.login.linkCopied" });
    } catch {
      linkAlert.current = pushAlert({ tone: "warning", key: "accounts.login.copyFailed" });
    }
  };

  const cancel = useCallback(async () => {
    if (flow) await dashboardApi.cancelDeviceLogin(flow.flowId).catch(() => undefined);
    setFlow(undefined);
    if (linkAlert.current !== undefined) dismissAlert(linkAlert.current);
  }, [flow]);

  // Registration is triggered by the approval itself, so the operator only ever
  // clicks once. The ref guards against a second poll landing mid-registration.
  const registerApproved = useCallback(
    async (flowId: string) => {
      if (registering.current) return;
      registering.current = true;
      try {
        const result = await dashboardApi.registerDeviceLogin(flowId);
        const registered = registrationMessage(language, result);
        pushAlert({ tone: registered.tone === "ok" ? "success" : "warning", text: registered.text });
        setFlow(undefined);
        await onRegistered();
      } catch (cause) {
        pushError(cause);
        setFlow(undefined);
      } finally {
        registering.current = false;
      }
    },
    [onRegistered, language],
  );

  useEffect(() => {
    if (!flow || flow.status !== "pending") return;

    let stopped = false;
    let timer: number | undefined;

    const tick = async () => {
      try {
        const next = await dashboardApi.pollDeviceLogin(flow.flowId);
        if (stopped) return;
        setFlow(next);
        if (next.status === "approved") {
          await registerApproved(next.flowId);
          return;
        }
        if (next.status !== "pending") {
          pushAlert(
            next.detail
              ? { tone: "error", error: next.detail }
              : { tone: "error", key: "accounts.login.status", vars: { status: next.status } },
          );
          setFlow(undefined);
          return;
        }
      } catch (cause) {
        if (stopped) return;
        pushError(cause);
        setFlow(undefined);
        return;
      }
      if (!stopped) timer = window.setTimeout(tick, POLL_INTERVAL_MS);
    };

    timer = window.setTimeout(tick, POLL_INTERVAL_MS);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [flow, registerApproved]);

  const pending = flow?.status === "pending";
  useEffect(() => {
    if (!pending) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [pending]);

  const remaining = Math.max(0, Math.ceil((deadline - now) / 1000));
  const remainingLabel = `${Math.floor(remaining / 60)}:${String(remaining % 60).padStart(2, "0")}`;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <LogIn size={16} aria-hidden /> {t("accounts.login.title")}
        </CardTitle>
        <CardDescription>
          {t("accounts.login.description")}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {flow && flow.status === "pending" ? (
          <div className="space-y-3 rounded-lg border p-4">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <p className="text-sm">
                {t("accounts.login.waiting", { time: remainingLabel })}
              </p>
              <Badge variant="secondary">{flow.provider}</Badge>
            </div>
            <a
              href={flow.verificationUriComplete}
              target="_blank"
              rel="noreferrer"
              className="block break-all font-mono text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
            >
              {flow.verificationUriComplete}
            </a>
            <div className="flex flex-wrap gap-2">
              <Button size="sm" variant="outline" onClick={() => void copyLink(flow.verificationUriComplete)}>
                <Link2 />
                {t("accounts.login.copyLink")}
              </Button>
              <Button size="sm" variant="outline" onClick={() => void cancel()}>
                <X />
                {t("accounts.cancel")}
              </Button>
            </div>
          </div>
        ) : (
          <div className="space-y-2">
            <div className="grid gap-2 sm:grid-cols-3">
              {PROVIDERS.map(({ id, label, mark: Mark }) => (
                <Button
                  key={id}
                  variant="outline"
                  disabled={busy}
                  onClick={() => void start(id)}
                  className="h-11 justify-center gap-2.5 font-medium"
                >
                  <Mark />
                  {t("accounts.login.continueWith", { provider: label })}
                </Button>
              ))}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
