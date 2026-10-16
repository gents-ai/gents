import { createStore, type StoreApi } from "zustand/vanilla";

import {
  reconcileProjectedWorkflow,
  type ChatWorkflowState,
  type OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";

import { loadChatFolders, type ChatFolders } from "./chatFolders";
import { createSelectors, type WithSelectors } from "./createSelectors";

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

export function releaseOwnedSubmissionWorkflow(
  current: ChatWorkflowState,
  owned: ChatWorkflowState,
): ChatWorkflowState {
  return current === owned ? READY : current;
}

/** A send's workflow and its sending flag change in one write, so no state
    between them is seen. */
export const chat = {
  beginSubmission(store: ChatStore, owned: ChatWorkflowState) {
    store.setState({ localWorkflow: owned, sending: true });
  },
  endSubmission(store: ChatStore, owned: ChatWorkflowState) {
    store.setState((state) => ({
      localWorkflow: releaseOwnedSubmissionWorkflow(state.localWorkflow, owned),
      sending: false,
    }));
  },
  awaitObservation(
    store: ChatStore,
    request: { nodeDid: string; sessionId: string; requestId: string },
  ) {
    store.setState({ localWorkflow: { kind: "awaitingObservation", ...request } });
  },
  resetWorkflow(store: ChatStore) {
    if (store.getState().localWorkflow !== READY)
      store.setState({ localWorkflow: READY });
  },
  followProjection(store: ChatStore, projected: ChatWorkflowState) {
    store.setState((state) => {
      const next = reconcileProjectedWorkflow(state.localWorkflow, projected);
      return next === state.localWorkflow ? state : { localWorkflow: next };
    });
  },
  showPendingTurn(store: ChatStore, turn: OptimisticPendingTurn) {
    store.setState({ optimisticPendingTurn: turn });
  },
  endPendingTurn(store: ChatStore, requestId: string) {
    if (store.getState().optimisticPendingTurn?.requestId === requestId)
      store.setState({ optimisticPendingTurn: null });
  },
};
