import { useEffect, useMemo, useState, type ReactNode } from "react";

import { createDesktopClient } from "@source-inc/gents-desktop-client";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { Toaster } from "@gents/ui/components/sonner";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { listen } from "@tauri-apps/api/event";

import { ErrorBoundary } from "./components/ErrorBoundary";
import { StartupScreen } from "./components/StartupScreen";
import { useMobileBackSwipe } from "./hooks/useMobileBackSwipe";
import { useMobileVisualViewport } from "./hooks/useMobileVisualViewport";
import type { DesktopShellBridge } from "./hooks/useDesktopShell";
import { installExternalLinkGuard } from "./lib/externalLinks";
import { startNativeSimulatorE2e } from "./lib/nativeSimulatorE2e";
import { isMobileTauriShell } from "./lib/shellPlatform";
import { applyShellPlatform } from "./lib/shellPlatform";
import { AppShell } from "./ui/app/AppShell";
import { BehaviorColorsContext } from "./ui/screens/behavior-colors";
import { AgentScreen } from "./ui/screens/agent/AgentScreen";
import { AgentsScreen } from "./ui/screens/AgentsScreen";
import { MailboxScreen } from "./ui/screens/MailboxScreen";
import { SessionScreen } from "./ui/screens/SessionScreen";
import { SessionsScreen } from "./ui/screens/SessionsScreen";
import { Shortcuts } from "./ui/screens/Shortcuts";
import { SetupScreen } from "./ui/screens/setup/SetupScreen";
import { useShell, type ShellBridge } from "./ui/hooks/useShell";
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
      supportsManagedServer: !isMobileTauriShell(),
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
  useEffect(() => {
    if (!bridge.api.stopManagedServer || !("__TAURI_INTERNALS__" in window)) return;
    let unlisten: (() => void) | undefined;
    void listen("desktop://managed-server-tray-stop", () => {
      void bridge.api.stopManagedServer?.(true);
    }).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => unlisten?.();
  }, [bridge.api]);

  const agent = shell.selectedDeployment?.agentPrincipal.displayName ?? null;
  const [setup, setSetup] = useState<"unknown" | "active" | "done">("unknown");

  const titlebar = (
    <div aria-hidden="true" className="titlebar-drag-region" data-tauri-drag-region />
  );

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
    shell.deployments.length === 0
  ) {
    setSetup("active");
  }
  if (setup === "active" || (setup === "unknown" && shell.snapshot === null)) {
    return setup === "active" ? (
      <>
        {titlebar}
        <TooltipProvider>
          <BehaviorColorsContext.Provider value={shell.behaviorColors}>
            <SetupScreen
              shell={shell}
              onDone={() => {
                setSetup("done");
                void shell.refreshSnapshot();
                navigate({ name: "session", sessionId: null });
              }}
            />
          </BehaviorColorsContext.Provider>
        </TooltipProvider>
      </>
    ) : null;
  }

  return (
    <>
      {titlebar}
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
