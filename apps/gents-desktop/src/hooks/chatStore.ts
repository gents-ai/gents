import { createStore, type StoreApi } from "zustand/vanilla";

import {
  reconcileProjectedWorkflow,
  type ChatWorkflowState,
  type OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";

import { turnKey, type SentTurn } from "./sentTurns";
import { loadChatFolders, type ChatFolders } from "./chatFolders";
import { createSelectors, type WithSelectors } from "./createSelectors";
import { loadGoalDrafts, type GoalDraft } from "./goalDrafts";
import { loadChatDrafts, type ChatDraft } from "./chatDrafts";

export type ChatState = {
  localWorkflow: ChatWorkflowState;
  sending: boolean;
  submissionError: {
    nodeDid: string;
    sessionId: string | null;
    message: string;

    requestId?: string;
  } | null;
  optimisticPendingTurn: OptimisticPendingTurn | null;

  sentTurns: readonly SentTurn[];
  folders: ChatFolders;
  goalDrafts: GoalDraft[];
  goalDraftError: string | null;
  goalCommandsInFlight: ReadonlySet<string>;
  chatDrafts: ChatDraft[];
  chatDraftError: string | null;

  sendDeadlineTick: number;

  storingSend: string | null;

  transcriptDraws: TranscriptDraws | null;
};

export type TranscriptDraws = {
  nodeDid: string | null;
  sessionId: string;
  requestIds: readonly string[];
};

/* enough for a run of sends queued behind one turn */
const SENT_TURNS_KEPT = 16;

export type ChatStore = WithSelectors<StoreApi<ChatState>>;

export function createChatStore(initial: Partial<ChatState> = {}) {
  return createSelectors(
    createStore<ChatState>(() => ({
      localWorkflow: { kind: "ready" },
      sending: false,
      submissionError: null,
      optimisticPendingTurn: null,
      sentTurns: [],
      folders: loadChatFolders(),
      ...loadGoalDrafts(),
      goalCommandsInFlight: new Set(),
      ...loadChatDrafts(),
      sendDeadlineTick: 0,
      storingSend: null,
      transcriptDraws: null,
      ...initial,
    })),
  );
}

function withSentTurn(sent: readonly SentTurn[], turn: SentTurn): readonly SentTurn[] {
  return sent.some(
    (held) => held.requestId === turn.requestId && held.sessionId === turn.sessionId,
  )
    ? sent
    : [...sent.slice(-(SENT_TURNS_KEPT - 1)), turn];
}

const READY: ChatWorkflowState = { kind: "ready" };

export function releaseOwnedSubmissionWorkflow(
  current: ChatWorkflowState,
  owned: ChatWorkflowState,
): ChatWorkflowState {
  return current === owned ? READY : current;
}

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
  /** the turn a person sent, drawn until the transcript holds it; `queued`
      when it waits behind a turn already running */
  showPendingTurn(store: ChatStore, turn: OptimisticPendingTurn, queued = false) {
    store.setState((state) => ({
      optimisticPendingTurn: turn,
      sentTurns: withSentTurn(state.sentTurns, {
        sessionId: turn.sessionId,
        requestId: turn.requestId,
        queued,
      }),
    }));
  },

  tookSentTurn(store: ChatStore, key: string) {
    store.setState((state) =>
      state.sentTurns.some((turn) => !turn.taken && turnKey(turn.requestId) === key)
        ? {
            sentTurns: state.sentTurns.map((turn) =>
              turnKey(turn.requestId) === key ? { ...turn, taken: true } : turn,
            ),
          }
        : state,
    );
  },

  noteSentTurn(store: ChatStore, turn: SentTurn) {
    store.setState((state) => {
      const sentTurns = withSentTurn(state.sentTurns, turn);
      return sentTurns === state.sentTurns ? state : { sentTurns };
    });
  },
  endPendingTurn(store: ChatStore, requestId: string) {
    if (store.getState().optimisticPendingTurn?.requestId === requestId)
      store.setState({ optimisticPendingTurn: null });
  },
};
