/* Whether a message can be sent right now, and why not, in the desktop's
   words (chat-shell's ChatBlockedReason and hintFor). The admission
   layers come first: client, agent, route, behavior; then the turn. */
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { behaviorReadiness } from "./behavior-readiness";

export type SendStatus =
  { kind: "ready" } | { kind: "disabled"; reason: string; hint: string };

export function sendStatus(input: {
  clientOnline: boolean;
  deployment: DeploymentView | null;
  behaviorId: string | null;
  sending: boolean;
  inFlight: boolean;
  turnState: string | null | undefined;
}): SendStatus {
  if (!input.clientOnline)
    return {
      kind: "disabled",
      reason: "clientOffline",
      hint: "Secure client is not running",
    };
  if (!input.deployment)
    return {
      kind: "disabled",
      reason: "agentNotSelected",
      hint: "Select an agent before sending",
    };
  if (!input.deployment.dialSucceeded || !input.deployment.chatSafe)
    return {
      kind: "disabled",
      reason: "routeNotReady",
      hint: input.deployment.lastError
        ? `Secure route to the agent is not ready: ${input.deployment.lastError}`
        : "Secure route to the agent is not ready",
    };
  const readiness = behaviorReadiness(input.deployment, input.behaviorId);
  if (!readiness.ready)
    return {
      kind: "disabled",
      reason: "behaviorUnavailable",
      hint: `The selected behavior is unavailable: ${readiness.reason}`,
    };
  if (input.sending)
    return {
      kind: "disabled",
      reason: "submittingRequest",
      hint: "Submitting request",
    };
  if (input.inFlight)
    return {
      kind: "disabled",
      reason: "awaitingTurnTerminality",
      hint:
        input.turnState === "waitingForClaim"
          ? "Waiting for the active turn to start"
          : "Turn still streaming",
    };
  return { kind: "ready" };
}

/* Reasons that pass on their own — the request being written, the runtime
   catching up — belong to the status line under the composer, which says
   "Sending…" or "Syncing…". The placeholder carries only a reason the
   person can act on; otherwise it stays the ordinary invitation. */
const TRANSIENT = new Set([
  "submittingRequest",
  "waitingForRequestObservation",
  "awaitingTurnTerminality",
  "inconsistentTurnObservation",
  "sessionMissingFromSnapshot",
]);

export function placeholderFor(status: SendStatus, fallback: string): string {
  if (status.kind !== "disabled" || TRANSIENT.has(status.reason)) return fallback;
  return status.hint;
}
