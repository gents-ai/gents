import type {
  GoalControlRequest,
  GoalControlResult,
} from "@source-inc/gents-desktop-client";

export function appliedGoalResult(request: GoalControlRequest): GoalControlResult {
  const first = request.action.kind === "create" ? `first-${request.commandId}` : null;
  return {
    goalId: "goal",
    sessionId: request.sessionId!,
    requestId: first,
    submissionReceipt: {
      clientDid: request.clientDid!,
      nodeDid: request.nodeDid,
      sessionId: request.sessionId!,
      reference: {
        commandId: request.commandId!,
        commandDigest: "sha256:" + "a".repeat(64),
        initialRequestId: first,
        requesterDid: request.clientDid!,
      },
    },
  };
}
