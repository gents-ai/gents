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

/** What the shell decides from its stores, as one store screens select from. */
export type ShellView = ShellProjection & {
  /** what the selected session is waiting on, if anything */
  loadingStatus: SessionLoadingStatus | null;
  /** the sent message, shown until the transcript holds a row for it */
  pendingTurn: OptimisticPendingTurn | null;
  /** the composer's draft, by session or by the new-session screen's node
      and agent */
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

/* the same inputs: each by identity, except the session header, which is
   rebuilt from the held read and compared by its fields */
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

/**
 * Keeps the view in step with the stores: recomputed when an input
 * changes, synchronously, so an action reading it right after a write sees
 * that write. A part that comes out equal, at any depth, keeps its previous
 * object, so a screen selecting it is not re-rendered; a streamed chunk changes no
 * input at all.
 */
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
