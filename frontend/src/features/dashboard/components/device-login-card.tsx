import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { ComponentType } from "react";
import { ExternalLink, Link2, LogIn, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { dashboardApi } from "../api";
import { dismissAlert, pushAlert, pushError } from "../alerts";
import { isBrowserCallback, usesBrowserSignIn, type SocialLoginMode } from "../browser-login";
import { registrationMessage } from "../device-login-result";
import type { BrowserLoginFlow, DeviceLoginFlow, DeviceLoginProvider } from "../types";
import { AwsMark, GithubMark, GoogleMark } from "./provider-marks";
import { usePreferences } from "../preferences";

const POLL_INTERVAL_MS = 2500;

const PROVIDERS: { id: DeviceLoginProvider; label: string; mark: ComponentType<{ size?: number }> }[] = [
  { id: "builder-id", label: "AWS Builder ID", mark: AwsMark },
  { id: "google", label: "Google", mark: GoogleMark },
  { id: "github", label: "GitHub", mark: GithubMark },
];

type ActiveFlow = { kind: "device"; flow: DeviceLoginFlow } | { kind: "browser"; flow: BrowserLoginFlow };

const flowApi = {
  device: {
    poll: dashboardApi.pollDeviceLogin,
    register: dashboardApi.registerDeviceLogin,
    cancel: dashboardApi.cancelDeviceLogin,
  },
  browser: {
    poll: dashboardApi.pollBrowserLogin,
    register: dashboardApi.registerBrowserLogin,
    cancel: dashboardApi.cancelBrowserLogin,
  },
};

export function DeviceLoginCard({ onRegistered }: { onRegistered: () => Promise<void> }) {
  const { t, language } = usePreferences();
  const [active, setActive] = useState<ActiveFlow>();
  const [mode, setMode] = useState<SocialLoginMode>("browser");
  const [busy, setBusy] = useState(false);
  const [pasted, setPasted] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const [deadline, setDeadline] = useState(0);
  const registering = useRef(false);
  const linkAlert = useRef<number | undefined>(undefined);
  const pasteId = useId();

  const copyLink = async (url: string) => {
    try {
      await navigator.clipboard.writeText(url);
      linkAlert.current = pushAlert({ tone: "success", key: "accounts.login.linkCopied" });
    } catch {
      linkAlert.current = pushAlert({ tone: "warning", key: "accounts.login.copyFailed" });
    }
  };

  const start = async (provider: DeviceLoginProvider) => {
    setBusy(true);
    setPasted("");
    try {
      if (usesBrowserSignIn(provider, mode)) {
        const started = await dashboardApi.startBrowserLogin(provider);
        setActive({ kind: "browser", flow: started });
        setDeadline(Date.now() + started.expiresInSeconds * 1000);
      } else {
        const started = await dashboardApi.startDeviceLogin(provider);
        setActive({ kind: "device", flow: started });
        setDeadline(Date.now() + started.expiresInSeconds * 1000);
        await copyLink(started.verificationUriComplete);
      }
      setNow(Date.now());
    } catch (cause) {
      pushError(cause);
    } finally {
      setBusy(false);
    }
  };

  const cancel = useCallback(async () => {
    if (active) await flowApi[active.kind].cancel(active.flow.flowId).catch(() => undefined);
    setActive(undefined);
    if (linkAlert.current !== undefined) dismissAlert(linkAlert.current);
  }, [active]);

  // Registration is triggered by the approval itself, so the operator only ever
  // clicks once. The ref guards against a second poll landing mid-registration.
  const registerApproved = useCallback(
    async (kind: ActiveFlow["kind"], flowId: string) => {
      if (registering.current) return;
      registering.current = true;
      try {
        const result = await flowApi[kind].register(flowId);
        const registered = registrationMessage(language, result);
        pushAlert({ tone: registered.tone === "ok" ? "success" : "warning", text: registered.text });
        setActive(undefined);
        await onRegistered();
      } catch (cause) {
        pushError(cause);
        setActive(undefined);
      } finally {
        registering.current = false;
      }
    },
    [onRegistered, language],
  );

  const settle = useCallback(
    async (next: ActiveFlow) => {
      setActive(next);
      if (next.flow.status === "approved") {
        await registerApproved(next.kind, next.flow.flowId);
        return true;
      }
      if (next.flow.status !== "pending") {
        pushAlert(
          next.flow.detail
            ? { tone: "error", error: next.flow.detail }
            : { tone: "error", key: "accounts.login.status", vars: { status: next.flow.status } },
        );
        setActive(undefined);
        return true;
      }
      return false;
    },
    [registerApproved],
  );

  const submitPasted = async () => {
    if (active?.kind !== "browser") return;
    setBusy(true);
    try {
      const next = await dashboardApi.completeBrowserLogin(active.flow.flowId, pasted.trim());
      await settle({ kind: "browser", flow: next });
    } catch (cause) {
      pushError(cause);
    } finally {
      setBusy(false);
    }
  };

  const pendingKind = active?.flow.status === "pending" ? active.kind : undefined;
  const pendingId = active?.flow.status === "pending" ? active.flow.flowId : undefined;

  useEffect(() => {
    if (!pendingKind || !pendingId) return;

    let stopped = false;
    let timer: number | undefined;

    const tick = async () => {
      try {
        const flow = await flowApi[pendingKind].poll(pendingId);
        if (stopped) return;
        const next = { kind: pendingKind, flow } as ActiveFlow;
        if (await settle(next)) return;
      } catch (cause) {
        if (stopped) return;
        pushError(cause);
        setActive(undefined);
        return;
      }
      if (!stopped) timer = window.setTimeout(tick, POLL_INTERVAL_MS);
    };

    timer = window.setTimeout(tick, POLL_INTERVAL_MS);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [pendingKind, pendingId, settle]);

  useEffect(() => {
    if (!pendingKind) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [pendingKind]);

  const remaining = Math.max(0, Math.ceil((deadline - now) / 1000));
  const remainingLabel = `${Math.floor(remaining / 60)}:${String(remaining % 60).padStart(2, "0")}`;

  const pendingBody = () => {
    if (!active || active.flow.status !== "pending") return null;
    const link = active.kind === "browser" ? active.flow.authorizationUrl : active.flow.verificationUriComplete;
    return (
      <div className="space-y-3 rounded-lg border p-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="text-sm">
            {t(active.kind === "browser" ? "accounts.login.waitingSignIn" : "accounts.login.waiting", {
              time: remainingLabel,
            })}
          </p>
          <Badge variant="secondary">{active.flow.provider}</Badge>
        </div>
        <a
          href={link}
          target="_blank"
          rel="noreferrer"
          className="block break-all font-mono text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
        >
          {link}
        </a>
        <div className="flex flex-wrap gap-2">
          {active.kind === "browser" ? (
            <Button size="sm" asChild>
              <a href={link} target="_blank" rel="noreferrer">
                <ExternalLink />
                {t("accounts.login.openSignIn")}
              </a>
            </Button>
          ) : null}
          <Button size="sm" variant="outline" onClick={() => void copyLink(link)}>
            <Link2 />
            {t("accounts.login.copyLink")}
          </Button>
          <Button size="sm" variant="outline" onClick={() => void cancel()}>
            <X />
            {t("accounts.cancel")}
          </Button>
        </div>
        {active.kind === "browser" ? (
          <form
            className="space-y-2 border-t pt-3"
            onSubmit={(event) => {
              event.preventDefault();
              void submitPasted();
            }}
          >
            <Label htmlFor={pasteId}>{t("accounts.login.pasteLabel")}</Label>
            <p className="text-xs text-muted-foreground">
              {t(active.flow.listening ? "accounts.login.pasteHint" : "accounts.login.pasteNotListening")}
            </p>
            <div className="flex flex-wrap gap-2 sm:flex-nowrap">
              <Input
                id={pasteId}
                value={pasted}
                onChange={(event) => setPasted(event.currentTarget.value)}
                placeholder={`${active.flow.callbackUri}?code=…`}
                autoComplete="off"
                spellCheck={false}
                className="font-mono text-xs"
              />
              <Button type="submit" size="sm" variant="outline" disabled={busy || !isBrowserCallback(pasted)}>
                {t("accounts.login.pasteSubmit")}
              </Button>
            </div>
          </form>
        ) : null}
      </div>
    );
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <LogIn size={16} aria-hidden /> {t("accounts.login.title")}
        </CardTitle>
        <CardDescription>
          {t(mode === "browser" ? "accounts.login.browserDescription" : "accounts.login.deviceDescription")}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {pendingBody() ?? (
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
            <Button
              variant="link"
              size="sm"
              className="h-auto px-0 text-xs"
              onClick={() => setMode(mode === "browser" ? "device" : "browser")}
            >
              {t(mode === "browser" ? "accounts.login.useDevice" : "accounts.login.useBrowser")}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
