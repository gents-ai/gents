import { render } from "@testing-library/react";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { useEffect, type ReactElement, type ReactNode } from "react";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

import { createDesktopApp, type DesktopApp } from "../src/hooks/desktopApp";
import type { SharingAdapter } from "../src/hooks/sharing";
import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import type { SelectionState } from "../src/hooks/selectionStore";
import { writeSession } from "../src/hooks/sessionStore";
import { AppProvider } from "../src/ui/app/AppContext";
import type { FlagId } from "../src/ui/lib/flags";
import { bindNav, type Route } from "../src/ui/lib/router";
import { resetPreferences } from "../src/ui/preferences";
import { deployment } from "./config-panel-wiring/fixtures";

export function node(overrides: Record<string, unknown> = {}) {
  return { ...deployment, sessions: [], mailboxItems: [], ...overrides };
}

export function publish(app: DesktopApp, deployments: unknown[]) {
  publishSnapshot(app, {
    bootstrap: app.stores.client.getState().snapshot?.bootstrap ?? {},
    client: { clientDid: "test-client", deployments },
  } as unknown as DesktopClientSnapshot);
}

export function publishSnapshot(app: DesktopApp, snapshot: DesktopClientSnapshot) {
  app.stores.client.setState({ snapshot });
  applyFleetSnapshot(app.stores.fleet, snapshot);
}

export function testApp({
  api = {},
  snapshot,
  deployments,
  session,
  selection,
  reportFailure,
  supportsManagedServer,
  sharing,
}: {
  api?: object;
  sharing?: SharingAdapter;
  supportsManagedServer?: boolean;
  snapshot?: unknown;
  reportFailure?: (message: string) => void;
  deployments?: unknown[];
  session?: DesktopSessionSnapshot | null;
  selection?: Partial<SelectionState>;
} = {}): DesktopApp {
  const app = createDesktopApp({
    api: api as DesktopApiAdapter,
    reportFailure,
    supportsManagedServer,
    sharing,
  });
  if (snapshot) publishSnapshot(app, snapshot as DesktopClientSnapshot);
  if (deployments) publish(app, deployments);
  app.stores.selection.setState({
    ...(session
      ? { nodeDid: session.nodeDid ?? null, sessionId: session.sessionId }
      : {}),
    ...selection,
  });
  if (session) writeSession(app.stores.session, session);
  return app;
}

/** Starts the test as if the app launched with these experimental features
    switched; the rest keep their defaults. */
export function withFlags(flags: Partial<Record<FlagId, boolean>>) {
  resetPreferences(
    Object.fromEntries(
      Object.entries(flags).map(([id, on]) => [id, on ? "on" : "off"]),
    ) as Partial<Record<FlagId, "on" | "off">>,
  );
}

export function withApp(app: DesktopApp) {
  return function AppWrapper({ children }: { children: ReactNode }) {
    return <AppProvider value={app}>{children}</AppProvider>;
  };
}

export function renderIn(app: DesktopApp, ui: ReactElement) {
  return render(ui, { wrapper: withApp(app) });
}

export function renderRouted(
  app: DesktopApp,
  initial: Route,
  page: (route: Route) => ReactElement | null,
) {
  let nav: Nav | null = null;
  function Routed() {
    nav = useNav();
    bindNav(nav);
    /* a later test's navigate must not drive this unmounted nav */
    useEffect(() => () => bindNav(null), []);
    /* the kit's nav carries the desktop's routes (the plugin page among them) */
    return page(nav.route as Route);
  }
  const view = renderIn(
    app,
    <MemoryNavProvider
      initial={initial as Parameters<typeof MemoryNavProvider>[0]["initial"]}
    >
      <Routed />
    </MemoryNavProvider>,
  );
  return { ...view, nav: () => nav! };
}
