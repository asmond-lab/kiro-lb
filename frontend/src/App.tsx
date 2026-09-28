import {
  Activity,
  CircleCheck,
  Coins,
  CreditCard,
  Wallet,
  Info,
  Palette,
  KeyRound,
  LayoutDashboard,
  ServerCog,
  Settings,
  ShieldCheck,
  TriangleAlert,
  Users,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { dashboardApi } from "@/features/dashboard/api";
import { exactTokens, formatTokens, summarizeUsage } from "@/features/dashboard/format";
import { creditTotals } from "@/features/dashboard/credit-totals";
import { deriveOverviewKpis } from "@/features/dashboard/overview-kpis";
import { useDashboard } from "@/features/dashboard/use-dashboard";
import { useTabHash } from "@/features/dashboard/use-tab-hash";
import { AccountsPanel } from "@/features/dashboard/components/accounts-panel";
import { ApiKeysPanel } from "@/features/dashboard/components/api-keys-panel";
import { InfoPanel } from "@/features/dashboard/components/info-panel";
import { SettingsPanel } from "@/features/dashboard/components/settings-panel";
import { CreateKeyDialog } from "@/features/dashboard/components/create-key-dialog";
import { DeviceLoginCard } from "@/features/dashboard/components/device-login-card";
import { LoginCard } from "@/features/dashboard/components/login-card";
import { RequestLogTable } from "@/features/dashboard/components/request-log-table";
import { RequestRateChart } from "@/features/dashboard/components/request-rate-chart";
import { TokenUsagePanel } from "@/features/dashboard/components/token-usage-panel";
import { AccountTokenPanel } from "@/features/dashboard/components/account-token-panel";
import { TotalRateChart } from "@/features/dashboard/components/total-rate-chart";
import { AppearancePanel } from "@/features/dashboard/components/appearance-panel";
import { usePreferences } from "@/features/dashboard/preferences";
import { AppHeader, KiroLogo, StatCard } from "@/features/dashboard/components/shell";
import { StatCardSkeleton } from "@/features/dashboard/components/skeletons";

// Quota moves slowly, so this is deliberately far apart: each tick is a real
// call to Kiro for every account.
const USAGE_REFRESH_MS = 5 * 60 * 1000;

const formatCreditTotal = (n: number) => n.toLocaleString(undefined, { maximumFractionDigits: 1 });

export default function App() {
  const dashboard = useDashboard();
  const { t } = usePreferences();
  const credits = useMemo(() => creditTotals(dashboard.accounts), [dashboard.accounts]);
  const [tab, selectTab] = useTabHash();
  const [isCreateKeyOpen, setIsCreateKeyOpen] = useState(false);
  const { overview, isLoading, isMutating, runAction, isAuthenticated, isLive, refreshUsageQuietly } =
    dashboard;
  // Totals are derived from the same per-key usage the API keys tab shows, so
  // the two views can never disagree.
  const totals = useMemo(() => summarizeUsage(dashboard.keyUsage), [dashboard.keyUsage]);
  const kpis = useMemo(
    () => (overview ? deriveOverviewKpis(dashboard.accounts, overview) : undefined),
    [dashboard.accounts, overview],
  );

  // Refresh quota on a timer, quietly: no spinner and no full reload, so panels
  // do not repaint. The button animation stays reserved for a manual refresh.
  const mutatingRef = useRef(isMutating);
  useEffect(() => {
    mutatingRef.current = isMutating;
  }, [isMutating]);
  useEffect(() => {
    if (!isAuthenticated || !isLive) return;
    const timer = window.setInterval(() => {
      // Read through a ref so a mutation does not re-arm the timer, which is
      // what turned one scheduled refresh into a burst of them.
      if (!mutatingRef.current) void refreshUsageQuietly();
    }, USAGE_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [isAuthenticated, isLive, refreshUsageQuietly]);

  if (!dashboard.isAuthenticated && isLoading) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-background text-muted-foreground">
        <div role="status" className="flex items-center gap-3 text-sm">
          <KiroLogo />
          <span>{t("loadingDashboard")}</span>
        </div>
      </div>
    );
  }

  if (!dashboard.isAuthenticated) {
    // A cold-start outage should not present as a silent login screen: surface
    // the non-auth failure the hook kept out of the auth error slot.
    return <LoginCard error={dashboard.error || dashboard.connectionError || ""} onSignIn={dashboard.signIn} />;
  }

  const createKey = async (name: string) => {
    const created = await dashboardApi.createApiKey(name);
    await dashboard.reload();
    return created.apiKey;
  };

  return (
    <div className="min-h-screen bg-background">
      <AppHeader
        isMutating={isMutating}
        isLive={dashboard.isLive}
        lastUpdatedAt={dashboard.lastUpdatedAt}
        onToggleLive={() => dashboard.setIsLive(!dashboard.isLive)}
        onRefresh={() => void runAction(dashboardApi.refreshUsage)}
        onSignOut={() => void dashboard.signOut()}
      />

      {dashboard.connectionError && (
        <div role="status" aria-live="polite" className="border-b border-warning/30 bg-warning/10 text-warning">
          <div className="mx-auto flex max-w-7xl 2xl:max-w-[100rem] items-center justify-between gap-3 px-4 py-2 text-sm sm:px-6">
            <span className="flex items-center gap-2 font-medium">
              <TriangleAlert size={15} aria-hidden />
              {t("overview.connectionLost")}
            </span>
            <Button variant="outline" size="sm" onClick={() => void dashboard.reload()}>
              {t("overview.retry")}
            </Button>
          </div>
        </div>
      )}

      {dashboard.actionError && (
        <div role="alert" className="border-b border-destructive/30 bg-destructive/10 text-destructive">
          <div className="mx-auto flex max-w-7xl 2xl:max-w-[100rem] items-center justify-between gap-3 px-4 py-2 text-sm sm:px-6">
            <span className="flex items-center gap-2">
              <TriangleAlert size={15} aria-hidden />
              {dashboard.actionError}
            </span>
            <Button
              variant="ghost"
              size="icon"
              className="size-8 shrink-0"
              aria-label={t("overview.dismissError")}
              onClick={dashboard.clearActionError}
            >
              <X aria-hidden />
            </Button>
          </div>
        </div>
      )}

      {dashboard.actionNotice && (
        <div role="status" aria-live="polite" className="border-b border-success/30 bg-success/10 text-success">
          <div className="mx-auto flex max-w-7xl 2xl:max-w-[100rem] items-center justify-between gap-3 px-4 py-2 text-sm sm:px-6">
            <span className="flex items-center gap-2">
              <CircleCheck size={15} aria-hidden />
              {dashboard.actionNotice}
            </span>
            <Button
              variant="ghost"
              size="icon"
              className="size-8 shrink-0"
              aria-label={t("overview.dismissNotice")}
              onClick={dashboard.clearActionNotice}
            >
              <X aria-hidden />
            </Button>
          </div>
        </div>
      )}

      <main className="mx-auto max-w-7xl 2xl:max-w-[100rem] p-4 sm:p-6">
        <Tabs value={tab} onValueChange={selectTab} className="space-y-6">
          <TabsList className="h-10 w-full">
            <TabsTrigger value="overview" className="gap-2 px-2 sm:px-3" title={t("overview")}>
              <LayoutDashboard aria-hidden />
              <span className="hidden sm:inline">{t("overview")}</span>
            </TabsTrigger>
            <TabsTrigger value="accounts" className="gap-2 px-2 sm:px-3" title={t("accounts")}>
              <Users aria-hidden />
              <span className="hidden sm:inline">{t("accounts")}</span>
            </TabsTrigger>
            <TabsTrigger value="keys" className="gap-2 px-2 sm:px-3" title={t("apiKeys")}>
              <KeyRound aria-hidden />
              <span className="hidden sm:inline">{t("apiKeys")}</span>
            </TabsTrigger>
            <TabsTrigger value="settings" className="gap-2 px-2 sm:px-3" title={t("settings")}>
              <Settings aria-hidden />
              <span className="hidden sm:inline">{t("settings")}</span>
            </TabsTrigger>
            <TabsTrigger value="theme" className="gap-2 px-2 sm:px-3" title={t("theme")}>
              <Palette aria-hidden />
              <span className="hidden sm:inline">{t("theme")}</span>
            </TabsTrigger>
            <TabsTrigger value="info" className="gap-2 px-2 sm:px-3" title={t("info")}>
              <Info aria-hidden />
              <span className="hidden sm:inline">{t("info")}</span>
            </TabsTrigger>
          </TabsList>

          <TabsContent value="overview" className="space-y-6">
            <section className="grid gap-4 md:grid-cols-2 xl:grid-cols-12 xl:[&>*]:col-span-4">
              {isLoading || !overview ? (
                Array.from({ length: 6 }).map((_, index) => <StatCardSkeleton key={index} />)
              ) : (
                <>
                  <StatCard
                    label={t("overview.totalTokens")}
                    value={<span title={exactTokens(totals.totalTokens)}>{formatTokens(totals.totalTokens)}</span>}
                    icon={<Coins size={15} />}
                  />
                  <StatCard label={t("overview.requests24h")} value={overview.requests24h.toLocaleString()} icon={<Activity size={15} />} />
                  <StatCard
                    label={t("overview.success24h")}
                    value={
                      <span
                        className={kpis?.success.isCritical ? "text-destructive" : undefined}
                        title={t("overview.successTitle", { ok: overview.successes24h.toLocaleString(), total: overview.requests24h.toLocaleString() })}
                      >
                        {kpis?.success.label}
                      </span>
                    }
                    icon={<ShieldCheck size={15} className={kpis?.success.isCritical ? "text-destructive" : undefined} />}
                  />
                  <StatCard
                    label={t("overview.routableAccounts")}
                    value={
                      <span
                        className={kpis?.routableAccounts.isCritical ? "text-destructive" : undefined}
                        title={t("overview.routableTitle", { n: kpis?.routableAccounts.count ?? 0, total: kpis?.routableAccounts.total ?? 0 })}
                      >
                        {kpis?.routableAccounts.count}/{kpis?.routableAccounts.total}
                      </span>
                    }
                    icon={
                      <ServerCog
                        size={15}
                        className={kpis?.routableAccounts.isCritical ? "text-destructive" : undefined}
                      />
                    }
                  />
                  <StatCard
                    label={t("overview.creditsUsed")}
                    value={<span title={t("overview.creditsAccounts", { n: credits.accounts })}>{formatCreditTotal(credits.used)}</span>}
                    icon={<CreditCard size={15} />}
                  />
                  <StatCard
                    label={t("overview.creditsAvailable")}
                    value={
                      <span title={t("overview.creditsAccounts", { n: credits.accounts })}>
                        {formatCreditTotal(credits.available)}
                        <span className="text-base text-muted-foreground"> / {formatCreditTotal(credits.limit)}</span>
                      </span>
                    }
                    icon={<Wallet size={15} />}
                  />
                </>
              )}
            </section>

            {/* Side by side once there is room for both: the rate chart answers
                what the pool is doing now, the donut what it has spent, and
                reading them together is the point of this tab. They stack below
                xl, where half a screen is too narrow for the donut and legend.
                Overview stays pool-wide; the per-account breakdown and its
                inferred limits live on the Accounts tab, where a limit applies. */}
            <section className="grid items-stretch gap-6 xl:grid-cols-2">
              <TotalRateChart rate={dashboard.rate} isLoading={isLoading} />
              <TokenUsagePanel keyUsage={dashboard.keyUsage} isLoading={isLoading} />
            </section>

            <RequestLogTable
              page={dashboard.logs}
              isLoading={isLoading || dashboard.isLogsLoading}
              model={dashboard.logModel}
              order={dashboard.logOrder}
              onLimitChange={dashboard.setLogLimit}
              onOffsetChange={dashboard.setLogOffset}
              onModelChange={dashboard.setLogModel}
              onOrderChange={dashboard.setLogOrder}
            />
          </TabsContent>

          <TabsContent value="accounts" className="space-y-6">
            <AccountsPanel
              accounts={dashboard.accounts}
              isLoading={isLoading}
              isMutating={isMutating}
              onDeleteAccount={(id) => void runAction(() => dashboardApi.deleteAccount(id))}
              onToggleAccount={(id, enabled) => void runAction(() => dashboardApi.setAccountEnabled(id, enabled))}
            />
            {/* Placed here rather than on Overview for the reason stated above:
                Overview stays pool-wide, and this is a per-account breakdown. It
                pairs with the quota column in the panel above - that one is what
                Kiro counts, this one is what the gateway measured. */}
            <AccountTokenPanel accountTokenUsage={dashboard.accountTokenUsage} isLoading={isLoading} />
            <RequestRateChart rate={dashboard.rate} isLoading={isLoading} />
            <DeviceLoginCard onRegistered={dashboard.reload} />
          </TabsContent>

          <TabsContent value="keys">
            <ApiKeysPanel
              apiKeys={dashboard.apiKeys}
              keyUsage={dashboard.keyUsage}
              isLoading={isLoading}
              isMutating={isMutating}
              onCreate={() => setIsCreateKeyOpen(true)}
              onDelete={(id) => void runAction(() => dashboardApi.deleteApiKey(id))}
              onRename={(id, name) => void runAction(() => dashboardApi.renameApiKey(id, name))}
            />
          </TabsContent>

          <TabsContent value="info">
            <InfoPanel
              overview={overview}
              accounts={dashboard.accounts}
              routableAccounts={kpis?.routableAccounts?.count}
              lastUpdatedAt={dashboard.lastUpdatedAt}
              isLive={dashboard.isLive}
              isCheckingUpdates={dashboard.isCheckingUpdates}
              onCheckUpdates={() => void dashboard.checkForUpdates()}
              isInstallingUpdate={dashboard.isInstallingUpdate}
              onInstallUpdate={(version) => void dashboard.installUpdate(version)}
            />
          </TabsContent>

          <TabsContent value="settings">
            <SettingsPanel onNotice={dashboard.notify} />
          </TabsContent>

          <TabsContent value="theme">
            <AppearancePanel />
          </TabsContent>
        </Tabs>
      </main>

      <CreateKeyDialog open={isCreateKeyOpen} onOpenChange={setIsCreateKeyOpen} onCreate={createKey} />
    </div>
  );
}
