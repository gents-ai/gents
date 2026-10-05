import { render } from "@testing-library/react";
import type { ReactElement, ReactNode } from "react";

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
import { deployment } from "./config-panel-wiring/fixtures";

/** A node as the bridge lists it, complete, with `overrides`. */
export function node(overrides: Record<string, unknown> = {}) {
  return { ...deployment, sessions: [], mailboxItems: [], ...overrides };
}

/** A read of the nodes, published as the client lifecycle would: the
    client's snapshot, and its fleet by key. */
export function publish(app: DesktopApp, deployments: unknown[]) {
  const snapshot = {
    bootstrap: {},
    client: { deployments },
  } as unknown as DesktopClientSnapshot;
  app.stores.client.setState({ snapshot });
  applyFleetSnapshot(app.stores.fleet, snapshot);
}

/**
 * A real app over `api`, not started: its stores hold `deployments` as
 * read, and `session` as the selected session's read, selected unless
 * `selection` says otherwise.
 */
export function testApp({
  api = {},
  deployments,
  session,
  selection,
  reportFailure,
}: {
  api?: object;
  /** where a failed action is shown; the app passes its toast */
  reportFailure?: (message: string) => void;
  deployments?: unknown[];
  session?: DesktopSessionSnapshot | null;
  selection?: Partial<SelectionState>;
} = {}): DesktopApp {
  const app = createDesktopApp({ api: api as DesktopApiAdapter, reportFailure });
  if (deployments) publish(app, deployments);
  app.stores.selection.setState({
    ...(session
      ? { agentDid: session.agentDid ?? null, sessionId: session.sessionId }
      : {}),
    ...selection,
  });
  if (session) writeSession(app.stores.session, session);
  return app;
}

/** Renders under `app`, as the root provides it: pass as render's wrapper. */
export function withApp(app: DesktopApp) {
  return function AppWrapper({ children }: { children: ReactNode }) {
    return <AppProvider value={app}>{children}</AppProvider>;
  };
}

/** Renders `ui` under `app`; a rerender stays under it. */
export function renderIn(app: DesktopApp, ui: ReactElement) {
  return render(ui, { wrapper: withApp(app) });
}
