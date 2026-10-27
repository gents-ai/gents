import { vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

import { createClientLifecycle } from "../src/hooks/clientLifecycle";
import { createLocalServer } from "../src/hooks/localServer";
import { createLocalServerStore } from "../src/hooks/localServerStore";
import { createChatStore, type ChatState } from "../src/hooks/chatStore";
import { createClientStore } from "../src/hooks/clientStore";
import { applyFleetSnapshot, createFleetStore } from "../src/hooks/fleetStore";
import {
  createSelectionStore,
  type SelectionState,
  type SelectionStore,
} from "../src/hooks/selectionStore";
import { createSessionStore } from "../src/hooks/sessionStore";
import type { ShellProjection, ShellStores } from "../src/hooks/shellProjection";

export function fleetFor(deployments: unknown[] = []) {
  const fleet = createFleetStore();
  applyFleetSnapshot(fleet, {
    bootstrap: {},
    client: { clientDid: "test-client", deployments },
  } as unknown as DesktopClientSnapshot);
  return fleet;
}

export function shellStores({
  deployments,
  selection = {},
}: {
  deployments?: unknown[];
  selection?: Partial<SelectionState>;
} = {}): ShellStores {
  const client = createClientStore();
  if (deployments)
    client.setState({
      snapshot: {
        bootstrap: {},
        client: { clientDid: "test-client", deployments },
      } as unknown as DesktopClientSnapshot,
    });
  return {
    selection: createSelectionStore(selection),
    session: createSessionStore(),
    fleet: fleetFor(deployments),
    client,
    chat: createChatStore({ folders: {} }),
  };
}

export function admittingProjection(
  agentId = "coding",
  blocked?: string,
): ShellProjection {
  const status = blocked
    ? { kind: "disabled", reason: "agentUnavailable", hint: blocked }
    : { kind: "ready" };
  return {
    agentReadiness: { kind: "ready", agentId },
    shellProjection: { nonEmptyContentSendStatus: status },
    retryShellProjection: { nonEmptyContentSendStatus: { kind: "ready" } },
  } as unknown as ShellProjection;
}

export function historyOf<K extends keyof ChatState>(stores: ShellStores, key: K) {
  const values: ChatState[K][] = [];
  stores.chat.subscribe((state, prev) => {
    if (state[key] !== prev[key]) values.push(state[key]);
  });
  return values;
}

export function lifecycleFor(
  api: object,
  {
    supportsManagedServer = false,
    selection,
  }: { supportsManagedServer?: boolean; selection?: SelectionStore } = {},
) {
  const stores = shellStores();
  if (selection) stores.selection = selection;
  stores.client.setState({
    startupPhase: supportsManagedServer
      ? "checking-managed-server"
      : "loading-configuration",
  });
  const errors: (string | null)[] = [];
  stores.client.subscribe((state, prev) => {
    if (state.error !== prev.error) errors.push(state.error);
  });
  const localServerStore = createLocalServerStore();
  const lifecycle = createClientLifecycle({
    api: api as DesktopApiAdapter,
    localServer: createLocalServer({
      api: api as DesktopApiAdapter,
      store: localServerStore,
      client: stores.client,
    }),
    supportsManagedServer,
    stores,
    refreshSession: vi.fn(async () => null),
  });
  void lifecycle.initializeDesktop();
  return {
    ...lifecycle,
    stores,
    localServerStore,
    errors,
    get snapshot() {
      return stores.client.getState().snapshot;
    },
    get startupPhase() {
      return stores.client.getState().startupPhase;
    },
    get starting() {
      return stores.client.getState().starting;
    },
  };
}
