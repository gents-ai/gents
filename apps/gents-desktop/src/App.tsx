import { useEffect, useState, type ReactNode } from "react";

import { createDesktopClient } from "@source-inc/gents-desktop-client";
import {
  MemoryNavProvider,
  useNav,
  type Nav,
  type Route as ShellRoute,
} from "@gents/shell";
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
import { NodeNavigationGuard } from "./ui/app/NodeNavigationGuard";
import { useOutsideLinks } from "./ui/app/useOutsideLinks";
import { usePlatformSetup, useWindowTitle } from "./ui/app/platform";
import {
  pluginWindowStart,
  useClosesWhenPluginGone,
  type PluginWindowStart,
} from "./ui/app/pluginWindow";
import { useFirstRun } from "./ui/app/useFirstRun";
import { WindowControls } from "./ui/app/WindowControls";
import { useDockVisit } from "./ui/app/workspace";
import { useHomeDid, useStartup } from "./ui/hooks/useClient";
import { useFleet } from "./ui/hooks/useFleet";
import { fleetNodes } from "./ui/lib/scope";
import { useFollowRoute } from "./ui/hooks/useFollowRoute";
import { defaultAgentOf } from "./ui/lib/agents";
import { isLocalNode, nodeSetUp } from "./ui/lib/firstRun";
import { useUxHost } from "./ui/hooks/useUxHost";
import { useHistoryInputs } from "./ui/lib/history-inputs";
import {
  bindHomeNode,
  bindNav,
  interceptNavClicks,
  navigate,
  useHistory,
  useRoute,
  type Route,
} from "./ui/lib/router";
import { useSwipeNav } from "./ui/lib/swipe-nav";
import { closeWindow } from "./lib/nativeShell";
import { AgentScreen } from "./ui/screens/agent/AgentScreen";
import { SettingsScreen } from "./ui/screens/settings/SettingsScreen";
import { AgentsScreen } from "./ui/screens/AgentsScreen";
import { MailboxScreen } from "./ui/screens/MailboxScreen";
import { PacksScreen } from "./ui/screens/PacksScreen";
import { PluginPageScreen } from "./ui/screens/PluginPageScreen";
import { PluginAccessPrompt } from "./ui/screens/PluginAccessPrompt";
import { SessionScreen } from "./ui/screens/SessionScreen";
import { SessionsScreen } from "./ui/screens/SessionsScreen";
import { SetupScreen } from "./ui/screens/setup/SetupScreen";
import { Shortcuts } from "./ui/screens/Shortcuts";
import { SearchPalette } from "./ui/screens/SearchPalette";
import { MailboxStack } from "./ui/screens/MailboxStack";
import "./ui/screens/surfaces";

import "./App.css";

/** `start`: a UX plugin's page in a window of its own, read from the
    address the shell opened the window at unless given */
function App({
  bridge,
  start: given,
}: { bridge?: DesktopBridge; start?: PluginWindowStart | null } = {}) {
  /* the window's address never changes: navigation is memory-only */
  const start = given === undefined ? pluginWindowStart(window.location.search) : given;
  /* the kit's memory history carries the desktop's own routes as they are */
  const initial = start
    ? ({ name: "plugin", pluginId: start.pluginId } as unknown as ShellRoute)
    : undefined;
  return (
    <ErrorBoundary>
      <MemoryNavProvider initial={initial}>
        <NavBinder>
          <AppHost bridge={bridge} start={start} />
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

function AppHost({
  bridge: given,
  start,
}: {
  bridge?: DesktopBridge;
  start: PluginWindowStart | null;
}) {
  const [bridge] = useState(() => given ?? defaultBridge());
  const [app] = useState(() => createDesktopApp({ reportFailure: toast, ...bridge }));
  return (
    <AppProvider value={app}>
      {start ? (
        <>
          <PluginWindowReactions app={app} bridge={bridge} start={start} />
          <PluginWindowBody pluginId={start.pluginId} />
        </>
      ) : (
        <>
          <AppReactions app={app} bridge={bridge} />
          <AppBody />
        </>
      )}
    </AppProvider>
  );
}

/* A plugin page's own window: the client and the plugin host, and the node
   it was opened from. Startup and recovery stay with the main window. */
function PluginWindowReactions({
  app,
  bridge,
  start,
}: {
  app: DesktopApp;
  bridge: DesktopBridge;
  start: PluginWindowStart;
}) {
  useClientRuntime(app, bridge.listenToUpdates);
  usePlatformSetup();
  useUxHost(app, bridge.listenToUpdates);
  const { selectNode } = app.actions;
  useEffect(() => {
    if (start.nodeDid) selectNode(start.nodeDid);
  }, [selectNode, start.nodeDid]);
  return null;
}

/* only the page: no Sidebar, Panel, composer or setup */
function PluginWindowBody({ pluginId }: { pluginId: string }) {
  const startup = useStartup();
  useClosesWhenPluginGone(pluginId, closeWindow);
  if (startup.phase !== "ready") return <StartupScreen />;
  return (
    <TooltipProvider>
      <div
        className="h-dvh min-h-0 bg-background text-foreground"
        data-testid="plugin-window"
      >
        <PluginPageScreen pluginId={pluginId} windowed />
      </div>
      <Toaster closeButton />
    </TooltipProvider>
  );
}

function AppReactions({ app, bridge }: { app: DesktopApp; bridge: DesktopBridge }) {
  const route = useRoute();
  useClientRuntime(app, bridge.listenToUpdates);
  useFollowRoute(route);
  useWindowTitle(route);
  useDockVisit(route);
  useMobileVisualViewport();
  usePlatformSetup();
  useManagedServerTrayControls(app);
  /* the UX plugin host and its doors: reads the stores, re-renders nothing */
  useUxHost(app, bridge.listenToUpdates);
  return <NodeNavigationGuard />;
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
  useEffect(() => bindHomeNode(homeDid), [homeDid]);
  useOutsideLinks(
    startup.phase === "ready" && firstRun.settled && firstRun.phase !== "active",
  );

  const titlebar = (
    <div className="titlebar-drag-region" data-tauri-drag-region>
      {isWindowsTauriShell() && <WindowControls />}
    </div>
  );

  if (startup.incompatibleHome.report) {
    return (
      <>
        {titlebar}
        <IncompatibleHomeScreen error={startup.error} home={startup.incompatibleHome} />
      </>
    );
  }

  if (firstRun.phase === "active") {
    return (
      <>
        {titlebar}
        <TooltipProvider>
          <SetupScreen
            initialStep={hasLocalNode ? "inference" : "welcome"}
            onDone={(done, nodeDid) => {
              firstRun.finish();
              const deployment = nodeDid
                ? done.client?.deployments.find((node) => node.nodeDid === nodeDid)
                : nodeSetUp(done);
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

  if (!firstRun.settled) {
    return (
      <>
        {titlebar}
        <StartupScreen
          configurationPending
          initConfigStatus={firstRun.initConfigStatus}
        />
      </>
    );
  }

  return (
    <TooltipProvider>
      <AppShell route={route} history={history}>
        <RouteScreen route={route} />
      </AppShell>
      <Toaster closeButton />
      {hasLocalNode && <PluginAccessPrompt />}
      <Shortcuts />
      <SearchPalette />
      <MailboxStack route={route} />
    </TooltipProvider>
  );
}

function RouteScreen({ route }: { route: Route }) {
  switch (route.name) {
    case "sessions":
      return <SessionsScreen entry={route} />;
    case "sessionsUnder":
      return <SessionsScreen under={route.session} entry={route} />;
    case "sessionsArchived":
      return <SessionsScreen archived entry={route} />;
    case "session":
      return <SessionScreen />;
    case "mailbox":
      return <MailboxScreen />;
    case "packs":
      return <PacksScreen item={route.item} review={route.review} doc={route.doc} />;
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
    case "plugin":
      return <PluginPageScreen pluginId={route.pluginId} />;
    case "settings":
      return <SettingsScreen section={route.section} />;
  }
}

export default App;
