import { createStore, type StoreApi } from "zustand/vanilla";

import {
  reconcileProjectedWorkflow,
  type ChatWorkflowState,
  type OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";

import { loadChatFolders, type ChatFolders } from "./chatFolders";
import { createSelectors, type WithSelectors } from "./createSelectors";

/** The compose side of a chat: the submission workflow the client runs
    locally, whether a send is in flight, the turn shown for a sent
    message until the transcript holds it, and each chat's folder. */
export type ChatState = {
  localWorkflow: ChatWorkflowState;
  sending: boolean;
  optimisticPendingTurn: OptimisticPendingTurn | null;
  folders: ChatFolders;
};

export type ChatStore = WithSelectors<StoreApi<ChatState>>;

export function createChatStore(initial: Partial<ChatState> = {}) {
  return createSelectors(
    createStore<ChatState>(() => ({
      localWorkflow: { kind: "ready" },
      sending: false,
      optimisticPendingTurn: null,
      folders: loadChatFolders(),
      ...initial,
    })),
  );
}

const READY: ChatWorkflowState = { kind: "ready" };

/** A submission's workflow once its send has ended: ready, unless something
    has already moved it on. */
export function releaseOwnedSubmissionWorkflow(
  current: ChatWorkflowState,
  owned: ChatWorkflowState,
): ChatWorkflowState {
  return current === owned ? READY : current;
}

/** The chat's changes. A send's workflow and its sending flag change in one
    write, so no state between them is seen. */
export const chat = {
  /** a send or retry starts, owning this workflow */
  beginSubmission(store: ChatStore, owned: ChatWorkflowState) {
    store.setState({ localWorkflow: owned, sending: true });
  },
  /** it ends: sending stops, and the workflow it owned is released */
  endSubmission(store: ChatStore, owned: ChatWorkflowState) {
    store.setState((state) => ({
      localWorkflow: releaseOwnedSubmissionWorkflow(state.localWorkflow, owned),
      sending: false,
    }));
  },
  /** the bridge accepted the request: wait for the transcript to show it */
  awaitObservation(
    store: ChatStore,
    request: { agentDid: string; sessionId: string; requestId: string },
  ) {
    store.setState({ localWorkflow: { kind: "awaitingObservation", ...request } });
  },
  /** nothing in flight */
  resetWorkflow(store: ChatStore) {
    if (store.getState().localWorkflow !== READY)
      store.setState({ localWorkflow: READY });
  },
  /** the workflow as the transcript now shows it */
  followProjection(store: ChatStore, projected: ChatWorkflowState) {
    store.setState((state) => {
      const next = reconcileProjectedWorkflow(state.localWorkflow, projected);
      return next === state.localWorkflow ? state : { localWorkflow: next };
    });
  },
  /** the turn a person sent, drawn until the transcript holds it */
  showPendingTurn(store: ChatStore, turn: OptimisticPendingTurn) {
    store.setState({ optimisticPendingTurn: turn });
  },
};
