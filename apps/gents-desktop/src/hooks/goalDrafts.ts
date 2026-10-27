import type {
  GoalControlRequest,
  GoalControlResult,
  GoalSubmissionReceipt,
} from "@source-inc/gents-desktop-client";
import type { ChatStore } from "./chatStore";

const PREFIX = "gents.chat.goalDraft.v1:";

export type GoalDraft = {
  clientDid: string;
  nodeDid: string;
  commandId: string;
  sessionId: string;
  selectedSessionId: string | null;
  queued: boolean;
  commandJson: string;
  receipt?: GoalSubmissionReceipt;
  result?: GoalControlResult;
  rejection?: { reason: string; receipt: GoalSubmissionReceipt };
};

const identifier = (value: unknown): value is string =>
  typeof value === "string" && value.trim().length > 0;
const object = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);

export function goalDraftKey(
  draft: Pick<GoalDraft, "clientDid" | "nodeDid" | "commandId">,
) {
  return PREFIX + JSON.stringify([draft.clientDid, draft.nodeDid, draft.commandId]);
}

function decode(value: unknown): GoalDraft {
  if (
    !object(value) ||
    !identifier(value.clientDid) ||
    !identifier(value.nodeDid) ||
    !identifier(value.commandId) ||
    !identifier(value.sessionId) ||
    !(value.selectedSessionId === null || identifier(value.selectedSessionId)) ||
    typeof value.commandJson !== "string"
  )
    throw new Error(
      "A saved Goal change could not be read. Its delivery is still unconfirmed.",
    );
  const draft: GoalDraft = {
    clientDid: value.clientDid,
    nodeDid: value.nodeDid,
    commandId: value.commandId,
    sessionId: value.sessionId,
    selectedSessionId: value.selectedSessionId,
    queued: value.queued === true,
    commandJson: value.commandJson,
  };
  if (value.receipt !== undefined && !object(value.receipt))
    throw new Error(
      "The saved Goal preparation reference could not be read. Check this history before submitting another change.",
    );
  if (object(value.receipt)) {
    validateGoalReceipt(draft, value.receipt as GoalSubmissionReceipt);
    draft.receipt = value.receipt as GoalSubmissionReceipt;
  }
  if (object(value.result)) {
    try {
      validateGoalResult(draft, value.result as GoalControlResult);
      draft.result = value.result as GoalControlResult;
    } catch {}
  }
  if (
    object(value.rejection) &&
    typeof value.rejection.reason === "string" &&
    object(value.rejection.receipt)
  ) {
    try {
      validateGoalReceipt(draft, value.rejection.receipt as GoalSubmissionReceipt);
      draft.rejection = {
        reason: value.rejection.reason,
        receipt: value.rejection.receipt as GoalSubmissionReceipt,
      };
    } catch {}
  }
  return draft;
}

export function loadGoalDrafts(): {
  goalDrafts: GoalDraft[];
  goalDraftError: string | null;
} {
  try {
    const goalDrafts: GoalDraft[] = [];
    for (let index = 0; index < localStorage.length; index++) {
      const key = localStorage.key(index);
      if (!key?.startsWith(PREFIX)) continue;
      const draft = decode(JSON.parse(localStorage.getItem(key) ?? "null"));
      if (goalDraftKey(draft) !== key)
        throw new Error("The saved Goal change identity is inconsistent.");
      goalDrafts.push(draft);
    }
    return { goalDrafts, goalDraftError: null };
  } catch (error) {
    return {
      goalDrafts: [],
      goalDraftError:
        error instanceof Error
          ? error.message
          : "Saved Goal changes could not be read.",
    };
  }
}

export function retainGoalDraft(
  request: GoalControlRequest,
  selectedSessionId: string | null,
  queued = false,
): GoalDraft {
  return decode({
    clientDid: request.clientDid,
    nodeDid: request.nodeDid,
    commandId: request.commandId,
    sessionId: request.sessionId,
    selectedSessionId,
    queued,
    commandJson: JSON.stringify(request),
  });
}

export function goalDraftCommand(draft: GoalDraft): GoalControlRequest {
  const request: unknown = JSON.parse(draft.commandJson);
  if (
    !object(request) ||
    request.clientDid !== draft.clientDid ||
    request.nodeDid !== draft.nodeDid ||
    request.commandId !== draft.commandId ||
    request.sessionId !== draft.sessionId
  )
    throw new Error(
      "The saved Goal input does not match its original identity. Check its delivery.",
    );
  if (
    !object(request.action) ||
    !["create", "edit", "pause", "resume", "stop"].includes(
      String(request.action.kind),
    ) ||
    (request.action.kind === "create" &&
      typeof request.action.objective !== "string") ||
    (request.action.kind === "edit" &&
      request.action.objective !== null &&
      typeof request.action.objective !== "string")
  )
    throw new Error(
      "The saved Goal input is unreadable. Check delivery to recover its original outcome.",
    );
  return request as GoalControlRequest;
}

/** A separate key per operation avoids losing another window's retained form.
 * Storage refusal must prevent dispatch; the native journal owns signed bytes. */
export function saveGoalDraft(store: ChatStore, draft: GoalDraft) {
  const key = goalDraftKey(draft);
  const previous = localStorage.getItem(key);
  if (
    previous !== null &&
    decode(JSON.parse(previous)).commandJson !== draft.commandJson
  )
    throw new Error("The original Goal input cannot be replaced.");
  localStorage.setItem(key, JSON.stringify(draft));
  const loaded = loadGoalDrafts();
  if (loaded.goalDraftError) throw new Error(loaded.goalDraftError);
  if (!loaded.goalDrafts.some((row) => goalDraftKey(row) === key))
    throw new Error("The Goal change could not be saved. No new change was submitted.");
  store.setState(loaded);
}

export function removeGoalDraft(store: ChatStore, draft: GoalDraft) {
  localStorage.removeItem(goalDraftKey(draft));
  store.setState(loadGoalDrafts());
}

export function goalDraftsFor(
  drafts: readonly GoalDraft[],
  clientDid: string | null,
  nodeDid: string | null,
  sessionId: string | null,
) {
  return drafts.filter(
    (draft) =>
      draft.clientDid === clientDid &&
      draft.nodeDid === nodeDid &&
      (draft.selectedSessionId === sessionId || draft.sessionId === sessionId),
  );
}

export function validateGoalReceipt(draft: GoalDraft, receipt: GoalSubmissionReceipt) {
  if (
    receipt?.clientDid !== draft.clientDid ||
    receipt.nodeDid !== draft.nodeDid ||
    receipt.sessionId !== draft.sessionId ||
    receipt.reference?.commandId !== draft.commandId
  )
    throw new Error("The Goal receipt belongs to another saved change.");
  if (
    !identifier(receipt.reference.commandDigest) ||
    !identifier(receipt.reference.requesterDid) ||
    !(
      receipt.reference.initialRequestId === null ||
      identifier(receipt.reference.initialRequestId)
    )
  )
    throw new Error("The Goal receipt has no complete signed preparation reference.");
  const previous =
    draft.receipt?.reference ??
    draft.result?.submissionReceipt?.reference ??
    draft.rejection?.receipt.reference;
  if (
    previous &&
    (previous.commandDigest !== receipt.reference.commandDigest ||
      previous.initialRequestId !== receipt.reference.initialRequestId ||
      previous.requesterDid !== receipt.reference.requesterDid)
  )
    throw new Error("The Goal receipt changed its original signed preparation.");
}

export function validateGoalResult(draft: GoalDraft, result: GoalControlResult) {
  if (result.sessionId !== draft.sessionId || !result.submissionReceipt)
    throw new Error(
      "The Goal outcome has no matching retained receipt. Check its delivery.",
    );
  validateGoalReceipt(draft, result.submissionReceipt);
  if (
    result.submissionReceipt.reference.initialRequestId &&
    result.requestId !== result.submissionReceipt.reference.initialRequestId
  )
    throw new Error("The Goal outcome changed its original request identity.");
}
