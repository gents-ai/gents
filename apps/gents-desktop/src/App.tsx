import { useEffect, useState, type ReactNode } from "react";

import { createDesktopClient } from "@source-inc/gents-desktop-client";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { Toaster } from "@gents/ui/components/sonner";
import { toast } from "sonner";
import { TooltipProvider } from "@gents/ui/components/tooltip";

import { ErrorBoundary } from "./components/ErrorBoundary";
import { IncompatibleHomeScreen } from "./components/IncompatibleHomeScreen";
import { StartupScreen } from "./components/StartupScreen";
import { createDesktopApp, type DesktopBridge } from "./hooks/desktopApp";
import { useDesktopRuntime } from "./hooks/useDesktopRuntime";
import { useManagedServerTrayControls } from "./hooks/useManagedServerTrayControls";
import { useMobileBackSwipe } from "./hooks/useMobileBackSwipe";
import { useMobileVisualViewport } from "./hooks/useMobileVisualViewport";
import { useNativeWindowReadiness } from "./hooks/useNativeWindowReadiness";
import {
  isMobileTauriShell,
  isWindowsTauriShell,
  supportsLocalManagedServer,
} from "./lib/shellPlatform";
import { AppProvider, useApp } from "./ui/app/AppContext";
import { AppShell } from "./ui/app/AppShell";
import { usePlatformSetup, useWindowTitle } from "./ui/app/platform";
import { useFirstRun } from "./ui/app/useFirstRun";
import { WindowControls } from "./ui/app/WindowControls";
import { dockScope, workspace } from "./ui/app/workspace";
import { useDeployments, useSnapshot, useStartup } from "./ui/hooks/useClient";
import { useFollowRoute } from "./ui/hooks/useFollowRoute";
import { defaultAgentOf } from "./ui/lib/agents";
import { isLocalAgent } from "./ui/lib/firstRun";
import { useHistoryInputs } from "./ui/lib/history-inputs";
import {
  bindNav,
  interceptNavClicks,
  navigate,
  useHistory,
  useRoute,
} from "./ui/lib/router";
import { useSwipeNav } from "./ui/lib/swipe-nav";
import { AgentScreen } from "./ui/screens/agent/AgentScreen";
import { AgentsScreen } from "./ui/screens/AgentsScreen";
import { MailboxScreen } from "./ui/screens/MailboxScreen";
import { PluginAccessPrompt } from "./ui/screens/PluginAccessPrompt";
import { SessionScreen } from "./ui/screens/SessionScreen";
import { SessionsScreen } from "./ui/screens/SessionsScreen";
import { SetupScreen } from "./ui/screens/setup/SetupScreen";
import { Shortcuts } from "./ui/screens/Shortcuts";
import "./ui/screens/surfaces";

import "./App.css";

function App({ bridge }: { bridge?: DesktopBridge } = {}) {
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

function defaultBridge(): DesktopBridge {
  const client = createDesktopClient();
  return {
    api: client.api,
    listenToUpdates: (handler) => client.transport.listenClientUpdated(handler),
    supportsManagedServer: supportsLocalManagedServer(),
  };
}

/* The app, made once for the window's life, and what it does on its own. */
function AppHost({ bridge: given }: { bridge?: DesktopBridge }) {
  const [bridge] = useState(() => given ?? defaultBridge());
  /* a failed action is reported once, as a toast where the person is */
  const [app] = useState(() => createDesktopApp({ reportFailure: toast, ...bridge }));
  useDesktopRuntime(app, bridge.listenToUpdates);
  useMobileVisualViewport();
  usePlatformSetup();
  useManagedServerTrayControls(bridge.api);
  return (
    <AppProvider value={app}>
      <AppBody managedServerSupported={bridge.supportsManagedServer === true} />
    </AppProvider>
  );
}

function AppBody({ managedServerSupported }: { managedServerSupported: boolean }) {
  const { api, lifecycle, actions } = useApp();
  const route = useRoute();
  const history = useHistory();
  /* the dock of each screen is found from its route while rendering; this
     only records the visit, which bounds how many docks are remembered */
  const scope = dockScope(route);
  useEffect(() => workspace.visit(scope), [scope]);
  useHistoryInputs(history);
  useSwipeNav(history);
  useFollowRoute(route);
  useWindowTitle(route);

  const snapshot = useSnapshot();
  const deployments = useDeployments();
  const startup = useStartup();
  const firstRun = useFirstRun(
    snapshot,
    startup.incompatibleHome.generation,
    startup.phase === "ready",
  );
  const homeDid = snapshot?.bootstrap.initAgentDid ?? null;
  const hasLocalAgent = deployments.some((deployment) =>
    isLocalAgent(deployment, homeDid),
  );
  useNativeWindowReadiness(startup.phase === "ready" && firstRun.settled);

  const titlebar = (
    <div className="titlebar-drag-region" data-tauri-drag-region>
      {isWindowsTauriShell() && <WindowControls />}
    </div>
  );

  /* A home this version cannot open is answered before anything else,
     whichever operation found it; the wizard restarts at welcome after. */
  if (startup.incompatibleHome.report) {
    return (
      <>
        {titlebar}
        <IncompatibleHomeScreen error={startup.error} home={startup.incompatibleHome} />
      </>
    );
  }

  /* First-run owns its own starting page. Do not swap it for the global
     startup screen or the wizard remounts at welcome after the server is up. */
  if (firstRun.phase === "active") {
    return (
      <>
        {titlebar}
        <TooltipProvider>
          <SetupScreen
            initialStep={hasLocalAgent ? "inference" : "welcome"}
            onDone={(done) => {
              firstRun.finish();
              const deployment = done.client?.deployments[0];
              if (deployment) {
                actions.selectAgent(deployment.agentDid);
                const behavior = defaultAgentOf(deployment) ?? deployment.behaviors[0];
                if (behavior) actions.selectBehavior(behavior.behaviorId);
              }
              void actions.refreshSnapshot().then(() => {
                navigate({ name: "session", sessionId: null });
              });
            }}
          />
        </TooltipProvider>
      </>
    );
  }

  if (startup.phase !== "ready") {
    return (
      <>
        {titlebar}
        <StartupScreen
          error={startup.error}
          managedServerSupported={managedServerSupported}
          onRetry={lifecycle.retryStartup}
          managedServerWait={startup.managedServerWait}
          diagnosticsHint={startup.diagnosticsHint}
          onSkipManagedServerWait={lifecycle.skipManagedServerWait}
          onRestartManagedServer={
            startup.canRestartManagedServer ? lifecycle.restartManagedServer : undefined
          }
          onOpenLoginItems={api.openManagedServerLoginItems}
          phase={startup.phase}
        />
      </>
    );
  }

  if (!firstRun.settled) return null;

  return (
    <TooltipProvider>
      <AppShell route={route} history={history}>
        {route.name === "sessions" && <SessionsScreen nodeDid={route.nodeDid} />}
        {route.name === "session" && <SessionScreen />}
        {route.name === "mailbox" && <MailboxScreen nodeDid={route.nodeDid} />}
        {(route.name === "agents" || route.name === "nodes") && <AgentsScreen />}
        {route.name === "agent" && (
          <AgentScreen
            agentDid={route.agentDid}
            section={route.section}
            item={route.item}
          />
        )}
      </AppShell>
      <Toaster />
      {hasLocalAgent && <PluginAccessPrompt />}
      <Shortcuts />
    </TooltipProvider>
  );
}

export default App;
