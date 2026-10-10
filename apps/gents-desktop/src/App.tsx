import { useEffect, useState, type ReactNode } from "react";

import { createDesktopClient } from "@source-inc/gents-desktop-client";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { Toaster } from "@gents/ui/components/sonner";
import { toast } from "sonner";
import { TooltipProvider } from "@gents/ui/components/tooltip";

import { ErrorBoundary } from "./components/ErrorBoundary";
import { IncompatibleHomeScreen } from "./components/IncompatibleHomeScreen";
import { StartupScreen } from "./components/StartupScreen";
import {
  createDesktopApp,
  type DesktopApp,
  type DesktopBridge,
} from "./hooks/desktopApp";
import { useClientRuntime } from "./hooks/useClientRuntime";
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
import { useDockVisit } from "./ui/app/workspace";
import { useHomeDid, useStartup } from "./ui/hooks/useClient";
import { useFleet } from "./ui/hooks/useFleet";
import { fleetNodes } from "./ui/lib/scope";
import { useFollowRoute } from "./ui/hooks/useFollowRoute";
import { defaultAgentOf } from "./ui/lib/agents";
import { isLocalNode, nodeSetUp } from "./ui/lib/firstRun";
import { useHistoryInputs } from "./ui/lib/history-inputs";
import {
  bindNav,
  interceptNavClicks,
  navigate,
  useHistory,
  useRoute,
  type Route,
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
  return (
    <AppProvider value={app}>
      <AppReactions app={app} bridge={bridge} />
      <AppBody />
    </AppProvider>
  );
}

/* What the app does on its own. Its hooks follow the stores, so they live
   in a leaf that draws nothing: their re-renders stop here instead of
   redrawing every screen. */
function AppReactions({ app, bridge }: { app: DesktopApp; bridge: DesktopBridge }) {
  const route = useRoute();
  useClientRuntime(app, bridge.listenToUpdates);
  useFollowRoute(route);
  useWindowTitle(route);
  useDockVisit(route);
  useMobileVisualViewport();
  usePlatformSetup();
  useManagedServerTrayControls(app);
  return null;
}

function AppBody() {
  const { actions } = useApp();
  const route = useRoute();
  const history = useHistory();
  useHistoryInputs(history);
  useSwipeNav(history);

  const startup = useStartup();
  const firstRun = useFirstRun(
    startup.incompatibleHome.generation,
    startup.phase === "ready",
  );
  const homeDid = useHomeDid();
  const hasLocalNode = useFleet((state) =>
    fleetNodes(state).some((node) => isLocalNode(node, homeDid)),
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
            initialStep={hasLocalNode ? "inference" : "welcome"}
            onDone={(done) => {
              firstRun.finish();
              const deployment = nodeSetUp(done);
              if (deployment) {
                actions.selectNode(deployment.nodeDid);
                const agent = defaultAgentOf(deployment) ?? deployment.agents[0];
                if (agent) actions.selectAgent(agent.agentId);
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
        <StartupScreen />
      </>
    );
  }

  if (!firstRun.settled) return null;

  return (
    <TooltipProvider>
      <AppShell route={route} history={history}>
        <RouteScreen route={route} />
      </AppShell>
      <Toaster />
      {hasLocalNode && <PluginAccessPrompt />}
      <Shortcuts />
    </TooltipProvider>
  );
}

/* The screen each route draws in the pane. */
function RouteScreen({ route }: { route: Route }) {
  switch (route.name) {
    case "sessions":
      return <SessionsScreen nodeDid={route.nodeDid} />;
    case "session":
      return <SessionScreen />;
    case "mailbox":
      return <MailboxScreen nodeDid={route.nodeDid} />;
    case "agents":
    case "nodes":
      return <AgentsScreen />;
    case "agent":
      return (
        <AgentScreen
          nodeDid={route.nodeDid}
          section={route.section}
          item={route.item}
        />
      );
  }
}

export default App;
