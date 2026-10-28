import type {
  DesktopApiAdapter,
  MailboxItemObservation,
} from "@source-inc/gents-desktop-client";
import {
  chatDraftKey,
  chatDraftRequest,
  loadChatDrafts,
  removeChatDraft,
  validateChatResult,
  type ChatDraft,
} from "./chatDrafts";
import type { ShellStores } from "./shellProjection";

export type AnswerCacheBinding = {
  clientDid: string;
  nodeDid: string;
  agentId: string;
  sessionId: string;
  itemId: string;
  sourceItemId: string;
  confirmed: boolean;
};
type AnswerObservation = Pick<MailboxItemObservation, "terminal"> & {
  item: Pick<
    MailboxItemObservation["item"],
    | "itemId"
    | "requesterDid"
    | "targetNodeDid"
    | "targetAgentId"
    | "sessionId"
    | "kind"
    | "action"
  >;
};

/** Only an exact native terminal observation plus an acknowledged, persisted
 * confirmation can retire an answer. Open-list absence provides no authority. */
export function mayRetireMailboxAnswer(
  binding: AnswerCacheBinding,
  observation: AnswerObservation | null,
  currentClient: string | null,
  acknowledged: boolean,
): boolean {
  const item = observation?.item;
  return Boolean(
    binding.confirmed &&
    acknowledged &&
    currentClient === binding.clientDid &&
    binding.sourceItemId === binding.itemId &&
    [
      binding.clientDid,
      binding.nodeDid,
      binding.agentId,
      binding.sessionId,
      binding.itemId,
    ].every((id) => id.length > 0) &&
    item &&
    observation?.terminal &&
    item.requesterDid === binding.clientDid &&
    item.targetNodeDid === binding.nodeDid &&
    item.targetAgentId === binding.agentId &&
    item.sessionId === binding.sessionId &&
    item.itemId === binding.itemId &&
    item.kind === "ask" &&
    item.action === "start_request",
  );
}

function bindingOf(draft: ChatDraft): AnswerCacheBinding | null {
  if (!draft.mailbox || !draft.result || draft.result.outcome != null) return null;
  try {
    validateChatResult(draft, draft.result);
    const request = chatDraftRequest(draft);
    return {
      clientDid: draft.clientDid,
      nodeDid: draft.nodeDid,
      agentId: request.agentId ?? "",
      sessionId: draft.sessionId,
      itemId: draft.mailbox.itemId,
      sourceItemId: request.causedBySourceDocId ?? "",
      confirmed: true,
    };
  } catch {
    return null;
  }
}

export function observeMailboxAnswerRetirement({
  api,
  stores,
}: {
  api: DesktopApiAdapter;
  stores: Pick<ShellStores, "client" | "chat" | "fleet">;
}) {
  let disposed = false;
  let scheduled = false;
  let generation = 0;
  const pending = new Map<string, { dirty: boolean }>();
  const clientDid = () => {
    const state = stores.client.getState();
    return state.starting || state.stopping
      ? null
      : (state.snapshot?.client?.clientDid ?? null);
  };
  function schedule() {
    if (disposed || scheduled) return;
    scheduled = true;
    queueMicrotask(() => {
      scheduled = false;
      if (!disposed) sweep();
    });
  }
  function sweep() {
    if (!clientDid() || typeof api.observeMailboxItem !== "function") return;
    const state = stores.chat.getState();
    for (const draft of state.chatDrafts) {
      const binding = bindingOf(draft);
      if (
        !binding ||
        binding.clientDid !== clientDid() ||
        state.chatRequestsInFlight.has(draft.requestId)
      )
        continue;
      if (
        stores.fleet
          .getState()
          .mailboxOf[draft.nodeDid]?.some(
            (item) => item.itemId === binding.itemId && item.status === "open",
          )
      )
        continue;
      const key = chatDraftKey(draft);
      const previous = pending.get(key);
      if (previous) {
        previous.dirty = true;
        continue;
      }
      const run = { dirty: false };
      pending.set(key, run);
      void retire(draft, binding)
        .catch(() => {
          // Failed reads or acknowledgements leave the original resolver intact.
        })
        .finally(() => {
          pending.delete(key);
          if (run.dirty) schedule();
        });
    }
  }
  async function retire(draft: ChatDraft, binding: AnswerCacheBinding) {
    const epoch = generation;
    const serialized = JSON.stringify(draft);
    const current = () => {
      if (disposed || generation !== epoch || clientDid() !== binding.clientDid)
        return false;
      const state = stores.chat.getState();
      if (state.chatRequestsInFlight.has(draft.requestId)) return false;
      const matches = (row: ChatDraft) =>
        chatDraftKey(row) === chatDraftKey(draft) && JSON.stringify(row) === serialized;
      const saved = loadChatDrafts();
      return (
        !saved.chatDraftError &&
        state.chatDrafts.some(matches) &&
        saved.chatDrafts.some(matches)
      );
    };
    const observation = await api.observeMailboxItem({
      clientDid: binding.clientDid,
      itemId: binding.itemId,
    });
    if (!current() || !mayRetireMailboxAnswer(binding, observation, clientDid(), true))
      return;
    await api.acknowledgeChatSubmission(draft.result!.submissionReceipt!);
    if (current() && mayRetireMailboxAnswer(binding, observation, clientDid(), true))
      removeChatDraft(stores.chat, draft);
  }
  const stop = [
    stores.client.subscribe((state, previous) => {
      if (
        state.snapshot?.client?.clientDid !== previous.snapshot?.client?.clientDid ||
        state.starting !== previous.starting ||
        state.stopping !== previous.stopping ||
        state.home.generation !== previous.home.generation
      ) {
        generation++;
        schedule();
      }
    }),
    stores.chat.subscribe((state, previous) => {
      if (
        state.chatDrafts !== previous.chatDrafts ||
        state.chatRequestsInFlight !== previous.chatRequestsInFlight
      )
        schedule();
    }),
    stores.fleet.subscribe((state, previous) => {
      if (state.mailboxOf !== previous.mailboxOf) schedule();
    }),
  ];
  schedule();
  return () => {
    disposed = true;
    generation++;
    stop.forEach((unsubscribe) => unsubscribe());
  };
}
