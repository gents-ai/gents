import { createStore, type StoreApi } from "zustand/vanilla";

import type { OptimisticPendingTurn } from "@source-inc/gents-desktop-chat";

import {
  projectSessionLoadingStatus,
  type SessionLoadState,
  type SessionLoadingStatus,
} from "../lib/loadingStatus";
import { equal, shareUnchanged } from "./fleetStore";
import {
  projectShell,
  projectionInputsOf,
  type ProjectionInputs,
  type ShellProjection,
  type ShellStores,
} from "./shellProjection";

export type ShellView = ShellProjection & {
  loadingStatus: SessionLoadingStatus | null;
  pendingTurn: OptimisticPendingTurn | null;
  draftKey: string;
};

export type ShellViewStore = StoreApi<ShellView>;

type ViewInputs = ProjectionInputs & {
  load: SessionLoadState;
  optimisticPendingTurn: OptimisticPendingTurn | null;
};

function inputsOf(stores: ShellStores): ViewInputs {
  const session = stores.session.getState();
  return {
    ...projectionInputsOf(stores),
    load: session.load,
    optimisticPendingTurn: stores.chat.getState().optimisticPendingTurn,
  };
}

function sameInputs(a: ViewInputs, b: ViewInputs) {
  return (Object.keys(b) as (keyof ViewInputs)[]).every((key) =>
    key === "session" ? equal(a.session, b.session) : a[key] === b[key],
  );
}

function viewOf(inputs: ViewInputs): ShellView {
  const projection = projectShell(inputs);
  const { selection, session, optimisticPendingTurn: turn } = inputs;
  return {
    ...projection,
    loadingStatus: projectSessionLoadingStatus({
      selectedSessionId: selection.sessionId,
      selectedNodeDid: selection.nodeDid,
      session,
      sessionLoad: inputs.load,
      operationalState: projection.operationalState,
    }),
    pendingTurn: turn,
    draftKey: JSON.stringify(
      selection.sessionId
        ? ["session", selection.nodeDid, selection.sessionId]
        : ["new", selection.nodeDid, projection.agentReadiness.agentId],
    ),
  };
}

export function createShellView(stores: ShellStores): ShellViewStore {
  let inputs = inputsOf(stores);
  const view = createStore<ShellView>(() => viewOf(inputs));
  const update = () => {
    const next = inputsOf(stores);
    if (sameInputs(inputs, next)) return;
    inputs = next;
    const prev = view.getState();
    const kept = shareUnchanged(prev, viewOf(next));
    if (kept !== prev) view.setState(kept, true);
  };
  stores.selection.subscribe(update);
  stores.session.subscribe(update);
  stores.fleet.subscribe(update);
  stores.client.subscribe(update);
  stores.chat.subscribe(update);
  return view;
}
