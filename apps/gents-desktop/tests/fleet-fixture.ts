import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { createChatStore, type ChatState } from "../src/hooks/chatStore";
import { createClientStore, type ClientStore } from "../src/hooks/clientStore";
import {
  applyFleetSnapshot,
  createFleetStore,
  type FleetStore,
} from "../src/hooks/fleetStore";
import { createSelectionStore, type SelectionState } from "../src/hooks/selectionStore";
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

/* one set of client stores per test, found by the test's api object, since
   a hook's options are rebuilt on every render */
const lifecycleStores = new WeakMap<
  object,
  { client: ClientStore; fleet: FleetStore }
>();
export function storesFor(api: object) {
  const found = lifecycleStores.get(api);
  if (found) return found;
  const stores = { client: createClientStore(), fleet: createFleetStore() };
  lifecycleStores.set(api, stores);
  return stores;
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
