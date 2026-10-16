import { render } from "@testing-library/react";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { useEffect, type ReactElement, type ReactNode } from "react";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

import { createDesktopApp, type DesktopApp } from "../src/hooks/desktopApp";
import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import type { SelectionState } from "../src/hooks/selectionStore";
import { writeSession } from "../src/hooks/sessionStore";
import { AppProvider } from "../src/ui/app/AppContext";
import { bindNav, type Route } from "../src/ui/lib/router";
import { deployment } from "./config-panel-wiring/fixtures";

export function node(overrides: Record<string, unknown> = {}) {
  return { ...deployment, sessions: [], mailboxItems: [], ...overrides };
}

export function publish(app: DesktopApp, deployments: unknown[]) {
  publishSnapshot(app, {
    bootstrap: app.stores.client.getState().snapshot?.bootstrap ?? {},
    client: { deployments },
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
}: {
  api?: object;
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

export function withApp(app: DesktopApp) {
  return function AppWrapper({ children }: { children: ReactNode }) {
    return <AppProvider value={app}>{children}</AppProvider>;
  };
}

export function renderIn(app: DesktopApp, ui: ReactElement) {
  return render(ui, { wrapper: withApp(app) });
}

/** Renders `page` for the route shown, starting at `initial`, under `app`
    and a memory nav that `navigate`, `back` and `replace` drive, as the
    app does. `nav()` is the nav as last rendered. */
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
    return page(nav.route);
  }
  const view = renderIn(
    app,
    <MemoryNavProvider initial={initial}>
      <Routed />
    </MemoryNavProvider>,
  );
  return { ...view, nav: () => nav! };
}
