import { useEffect, useMemo, useState, type ReactNode } from "react";

import { createDesktopClient } from "@source-inc/gents-desktop-client";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { Toaster } from "@gents/ui/components/sonner";
import { toast } from "sonner";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { ErrorBoundary } from "./components/ErrorBoundary";
import { StartupScreen } from "./components/StartupScreen";
import { useMobileBackSwipe } from "./hooks/useMobileBackSwipe";
import { useMobileVisualViewport } from "./hooks/useMobileVisualViewport";
import { useNativeWindowReadiness } from "./hooks/useNativeWindowReadiness";
import { useManagedServerTrayControls } from "./hooks/useManagedServerTrayControls";
import type { DesktopShellBridge } from "./hooks/useDesktopShell";
import { installExternalLinkGuard } from "./lib/externalLinks";
import { startNativeSimulatorE2e } from "./lib/nativeSimulatorE2e";
import {
  applyShellPlatform,
  isMobileTauriShell,
  isMacTauriShell,
  isWindowsTauriShell,
  supportsLocalManagedServer,
} from "./lib/shellPlatform";
import { AppShell } from "./ui/app/AppShell";
import { WindowControls } from "./ui/app/WindowControls";
import { BehaviorColorsContext } from "./ui/screens/behavior-colors";
import { AgentScreen } from "./ui/screens/agent/AgentScreen";
import { AgentsScreen } from "./ui/screens/AgentsScreen";
import { MailboxScreen } from "./ui/screens/MailboxScreen";
import { SessionScreen } from "./ui/screens/SessionScreen";
import { SessionsScreen } from "./ui/screens/SessionsScreen";
import { Shortcuts } from "./ui/screens/Shortcuts";
import { SetupScreen } from "./ui/screens/setup/SetupScreen";
import { useShell, type ShellBridge } from "./ui/hooks/useShell";
import { isLocalAgent, needsFirstRunSetup } from "./ui/lib/firstRun";
import { bindNav, interceptNavClicks, navigate, useRoute } from "./ui/lib/router";
import { initTheme } from "./ui/theme";

import "./App.css";

function NavBinder({ children }: { children: ReactNode }) {
  const nav = useNav();
  bindNav(nav);
  useEffect(() => interceptNavClicks(), []);
  return <BackSwipe nav={nav}>{children}</BackSwipe>;
}

function BackSwipe({ nav, children }: { nav: Nav; children: ReactNode }) {
  useMobileBackSwipe(isMobileTauriShell(), () => nav.back());
  return children;
}

function App({ bridge }: { bridge?: DesktopShellBridge } = {}) {
  return (
    <ErrorBoundary>
      <MemoryNavProvider>
        <NavBinder>
          <AppHost bridge={bridge} />
        </NavBinder>
      </MemoryNavProvider>
    </ErrorBoundary>
  );
}

function AppHost({ bridge: explicitBridge }: { bridge?: DesktopShellBridge }) {
  useMobileVisualViewport();
  const defaultBridge = useMemo<ShellBridge>(() => {
    const client = createDesktopClient();
    return {
      api: client.api,
      listenToUpdates: (handler) => client.transport.listenClientUpdated(handler),
      supportsManagedServer: supportsLocalManagedServer(),
    };
  }, []);
  const bridge = explicitBridge ?? defaultBridge;
  const route = useRoute();
  const shell = useShell(
    bridge,
    route.name === "session" ? route.sessionId : undefined,
  );

  useEffect(() => {
    initTheme();
    applyShellPlatform();
  }, []);
  useEffect(() => installExternalLinkGuard(document), []);
  useEffect(() => {
    void startNativeSimulatorE2e();
  }, []);
  useManagedServerTrayControls(bridge.api);
  const agent = shell.selectedDeployment?.agentPrincipal.displayName ?? null;
  useEffect(() => {
    if (!isMacTauriShell()) return;
    const title =
      route.name === "session"
        ? shell.selectedSession?.title || "New Session"
        : route.name === "sessions"
          ? "Sessions"
          : route.name === "agents"
            ? "Agents"
            : route.name === "mailbox"
              ? "Mailbox"
              : "Configuration";
    void getCurrentWindow().setTitle(
      agent ? `${title} — ${agent}` : `${title} — Gents`,
    );
  }, [agent, route.name, shell.selectedSession?.title]);
  const [setup, setSetup] = useState<"unknown" | "active" | "done">("unknown");
  useNativeWindowReadiness(
    shell.startupPhase === "ready" &&
      (setup === "done" ||
        (setup === "unknown" &&
          shell.snapshot !== null &&
          !needsFirstRunSetup(shell.snapshot))),
  );

  const titlebar = (
    <div className="titlebar-drag-region" data-tauri-drag-region>
      {isWindowsTauriShell() && <WindowControls />}
    </div>
  );

  /* First-run owns its own starting page. Do not swap it for the global
     startup screen or the wizard remounts at welcome after the server is up. */
  if (setup === "active") {
    const hasLocalAgent = shell.deployments.some((deployment) =>
      isLocalAgent(deployment, shell.snapshot?.bootstrap.initAgentDid),
    );
    return (
      <>
        {titlebar}
        <TooltipProvider>
          <BehaviorColorsContext.Provider value={shell.behaviorColors}>
            <SetupScreen
              shell={shell}
              initialStep={hasLocalAgent ? "inference" : "welcome"}
              onDone={(snapshot) => {
                setSetup("done");
                const deployment = snapshot.client?.deployments[0];
                if (deployment) {
                  shell.selectAgent(deployment.agentDid);
                  const behavior =
                    deployment.behaviors.find((row) => row.isDefault) ??
                    deployment.behaviors[0];
                  if (behavior) shell.selectBehavior(behavior.behaviorId);
                }
                void shell.refreshSnapshot().then(() => {
                  navigate({ name: "session", sessionId: null });
                });
              }}
            />
          </BehaviorColorsContext.Provider>
        </TooltipProvider>
      </>
    );
  }

  if (shell.startupPhase && shell.startupPhase !== "ready") {
    return (
      <>
        {titlebar}
        <StartupScreen
          error={shell.error}
          managedServerSupported={bridge.supportsManagedServer === true}
          onRetry={shell.reconnect}
          phase={shell.startupPhase}
        />
      </>
    );
  }

  if (
    setup === "unknown" &&
    shell.snapshot !== null &&
    needsFirstRunSetup(shell.snapshot)
  ) {
    setSetup("active");
    return null;
  }
  if (setup === "unknown" && shell.snapshot === null) {
    return null;
  }

  const openDbExplorer = shell.api.openDbExplorer
    ? () => {
        void shell.api.openDbExplorer?.().catch((e: unknown) => {
          toast(`DB explorer failed to open: ${String(e)}`);
        });
      }
    : null;

  return (
    <>
      <TooltipProvider>
        <BehaviorColorsContext.Provider value={shell.behaviorColors}>
          <AppShell
            route={route}
            agentName={agent}
            agentDid={shell.selectedAgentDid}
            deployment={shell.selectedDeployment}
            root={shell.snapshot?.bootstrap.initToolRoot}
            ceiling={shell.snapshot?.bootstrap.initToolCeiling}
            online={Boolean(shell.snapshot?.client)}
            mailboxCount={
              shell.selectedDeployment?.mailboxItems.filter((m) => m.status === "open")
                .length ?? 0
            }
            holds={
              new Set(shell.holds.flatMap((h) => (h.sessionId ? [h.sessionId] : [])))
            }
            syncHealth={shell.snapshot?.client?.syncHealth}
            error={shell.error}
            onDismissError={shell.clearError}
            onReconnect={shell.reconnect}
            onOpenDbExplorer={openDbExplorer}
          >
            {route.name === "sessions" && <SessionsScreen shell={shell} />}
            {route.name === "session" && <SessionScreen shell={shell} />}
            {route.name === "mailbox" && <MailboxScreen shell={shell} />}
            {route.name === "agents" && <AgentsScreen shell={shell} />}
            {route.name === "agent" && (
              <AgentScreen
                shell={shell}
                agentDid={route.agentDid}
                section={route.section}
                item={route.item}
              />
            )}
          </AppShell>
          <Toaster />
          <Shortcuts shell={shell} />
        </BehaviorColorsContext.Provider>
      </TooltipProvider>
    </>
  );
}

export default App;
