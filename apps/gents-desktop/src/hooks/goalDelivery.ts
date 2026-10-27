import {
  asBridgeErrorPayload,
  type DesktopApiAdapter,
  type GoalControlRequest,
  type GoalControlResult,
} from "@source-inc/gents-desktop-client";
import {
  goalDraftCommand,
  goalDraftsFor,
  loadGoalDrafts,
  removeGoalDraft,
  retainGoalDraft,
  saveGoalDraft,
  validateGoalReceipt,
  validateGoalResult,
  type GoalDraft,
} from "./goalDrafts";
import type { ShellStores } from "./shellProjection";

export function createGoalDelivery(api: DesktopApiAdapter, stores: ShellStores) {
  const busy = new Set<string>();
  function track(commandId: string, running: boolean) {
    stores.chat.setState((state) => {
      const goalCommandsInFlight = new Set(state.goalCommandsInFlight);
      if (running) goalCommandsInFlight.add(commandId);
      else goalCommandsInFlight.delete(commandId);
      return { goalCommandsInFlight };
    });
  }
  const clientDid = () => {
    const value = stores.client.getState().snapshot?.client?.clientDid;
    if (!value) throw new Error("Connect this client before changing a Goal.");
    return value;
  };
  function load() {
    const state = loadGoalDrafts();
    stores.chat.setState(state);
    if (state.goalDraftError) throw new Error(state.goalDraftError);
    return state.goalDrafts;
  }
  function original(commandId: string) {
    const draft = load().find(
      (row) => row.commandId === commandId && row.clientDid === clientDid(),
    );
    if (!draft)
      throw new Error("The original Goal change is unavailable for this client.");
    return draft;
  }
  async function query(draft: GoalDraft) {
    const recovered = await api.recoverGoalSubmission({
      clientDid: draft.clientDid,
      nodeDid: draft.nodeDid,
      commandId: draft.commandId,
    });
    if (recovered.kind === "unconfirmed") {
      if (
        recovered.nodeDid !== draft.nodeDid ||
        recovered.commandId !== draft.commandId
      )
        throw new Error("The Goal delivery check belongs to another change.");
    } else {
      validateGoalReceipt(draft, recovered.receipt);
      if (recovered.outcome?.kind === "applied") {
        validateGoalResult(draft, recovered.outcome.result);
        validateGoalReceipt(
          { ...draft, receipt: recovered.receipt },
          recovered.outcome.result.submissionReceipt!,
        );
        if (
          recovered.outcome.result.submissionReceipt?.reference.commandDigest !==
          recovered.receipt.reference.commandDigest
        )
          throw new Error("The Goal result differs from its retained receipt.");
      }
    }
    return recovered;
  }
  function settle(
    draft: GoalDraft,
    result: GoalControlResult,
    applied: (result: GoalControlResult, draft: GoalDraft) => boolean,
  ) {
    validateGoalResult(draft, result);
    saveGoalDraft(stores.chat, {
      ...draft,
      result,
      receipt: result.submissionReceipt!,
      rejection: undefined,
    });
    const consumed = applied(result, draft);
    void Promise.resolve()
      .then(() => api.acknowledgeGoalSubmission(result.submissionReceipt!))
      .then(() => {
        if (consumed) removeGoalDraft(stores.chat, draft);
      })
      .catch(() => {});
    return result;
  }
  async function rejected(draft: GoalDraft) {
    const found = await query(draft);
    if (found.kind !== "prepared" || found.outcome?.kind !== "rejected") return false;
    saveGoalDraft(stores.chat, {
      ...draft,
      receipt: found.receipt,
      rejection: { reason: found.outcome.reason, receipt: found.receipt },
    });
    return true;
  }
  return {
    async submit(
      request: GoalControlRequest,
      selectedSessionId: string | null,
      applied: (result: GoalControlResult, draft: GoalDraft) => boolean,
      queued = false,
    ) {
      const actor = clientDid();
      if (request.clientDid && request.clientDid !== actor)
        throw new Error("The selected client changed before dispatch.");
      const scope = JSON.stringify([actor, request.nodeDid, selectedSessionId]);
      if (
        busy.has(scope) ||
        goalDraftsFor(load(), actor, request.nodeDid, selectedSessionId).length
      )
        throw new Error(
          "Check the saved Goal change before submitting another change in this chat.",
        );
      const draft = retainGoalDraft(
        {
          ...request,
          clientDid: actor,
          commandId: crypto.randomUUID(),
          sessionId: request.sessionId ?? crypto.randomUUID(),
        },
        selectedSessionId,
        queued,
      );
      busy.add(scope);
      track(draft.commandId, true);
      try {
        saveGoalDraft(stores.chat, draft);
        if (clientDid() !== actor)
          throw new Error("The selected client changed before dispatch.");
        return settle(draft, await api.controlGoal(goalDraftCommand(draft)), applied);
      } catch (error) {
        if (asBridgeErrorPayload(error)?.code === "goalControlRejected")
          await rejected(draft).catch(() => {});
        throw error;
      } finally {
        busy.delete(scope);
        track(draft.commandId, false);
      }
    },
    async recover(
      commandId: string,
      resume: boolean,
      applied: (result: GoalControlResult, draft: GoalDraft) => boolean,
    ) {
      const draft = original(commandId);
      if (stores.chat.getState().goalCommandsInFlight.has(commandId))
        throw new Error("This Goal change is still being submitted or checked.");
      track(commandId, true);
      try {
        const recovered = await query(draft);
        if (recovered.kind === "prepared")
          saveGoalDraft(stores.chat, { ...draft, receipt: recovered.receipt });
        if (recovered.kind === "prepared" && recovered.outcome?.kind === "applied")
          return settle(draft, recovered.outcome.result, applied);
        if (recovered.kind === "prepared" && recovered.outcome?.kind === "rejected") {
          saveGoalDraft(stores.chat, {
            ...draft,
            receipt: recovered.receipt,
            rejection: { reason: recovered.outcome.reason, receipt: recovered.receipt },
          });
          return null;
        }
        if (!resume) return null;
        if (clientDid() !== draft.clientDid)
          throw new Error(
            "The selected client changed while delivery was being checked.",
          );
        const request = goalDraftCommand(draft);
        const result =
          recovered.kind === "prepared"
            ? await api.resumeGoalSubmission(request, recovered.receipt)
            : draft.receipt ||
                draft.result?.submissionReceipt ||
                draft.rejection?.receipt
              ? await api.resumeGoalSubmission(
                  request,
                  (draft.receipt ??
                    draft.result?.submissionReceipt ??
                    draft.rejection?.receipt)!,
                )
              : await api.controlGoal(request);
        return settle(draft, result, applied);
      } finally {
        track(commandId, false);
      }
    },
    async dismiss(commandId: string) {
      const draft = original(commandId);
      const recovered = await query(draft);
      if (recovered.kind !== "prepared" || recovered.outcome?.kind !== "rejected")
        throw new Error(
          "Check the saved change before dismissing it; its outcome is not a confirmed refusal.",
        );
      removeGoalDraft(stores.chat, draft);
      void Promise.resolve()
        .then(() => api.acknowledgeGoalSubmission(recovered.receipt))
        .catch(() => {});
    },
  };
}
