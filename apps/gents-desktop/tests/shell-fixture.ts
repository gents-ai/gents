import { vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

import { createClientLifecycle } from "../src/hooks/clientLifecycle";
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

/** A fleet store holding `deployments`, as a shell's fleet. */
export function fleetFor(deployments: unknown[] = []) {
  const fleet = createFleetStore();
  applyFleetSnapshot(fleet, {
    bootstrap: {},
    client: { deployments },
  } as unknown as DesktopClientSnapshot);
  return fleet;
}

/** Every shell store, holding `deployments` as both the client's read and
    its fleet, with the selection given. */
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
        client: { deployments },
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

/** A projection admitting a send and a retry under `behaviorId`, or one
    whose send status is `blocked`, for actions tested apart from the
    stores' own projection. */
export function admittingProjection(
  behaviorId = "coding",
  blocked?: string,
): ShellProjection {
  const status = blocked
    ? { kind: "disabled", reason: "behaviorUnavailable", hint: blocked }
    : { kind: "ready" };
  return {
    behaviorReadiness: { kind: "ready", behaviorId },
    shellProjection: { nonEmptyContentSendStatus: status },
    retryShellProjection: { nonEmptyContentSendStatus: { kind: "ready" } },
  } as unknown as ShellProjection;
}

/** Each value `key` takes in the chat store, in order. */
export function historyOf<K extends keyof ChatState>(stores: ShellStores, key: K) {
  const values: ChatState[K][] = [];
  stores.chat.subscribe((state, prev) => {
    if (state[key] !== prev[key]) values.push(state[key]);
  });
  return values;
}

/** A started lifecycle over fresh stores, as the app makes it, with the
    client's state read through getters and every banner error kept. */
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
  const lifecycle = createClientLifecycle({
    api: api as DesktopApiAdapter,
    supportsManagedServer,
    stores,
    refreshSession: vi.fn(async () => null),
  });
  void lifecycle.initializeDesktop();
  return {
    ...lifecycle,
    stores,
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
