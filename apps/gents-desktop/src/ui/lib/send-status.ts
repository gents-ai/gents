import type { ChatBlockedReason, SendStatus } from "@source-inc/gents-desktop-chat";

const TRANSIENT = new Set<ChatBlockedReason>([
  "submittingRequest",
  "waitingForRequestObservation",
  "inconsistentTurnObservation",
  "sessionMissingFromSnapshot",
]);

export function placeholderFor(status: SendStatus, fallback: string): string {
  if (
    status.kind !== "disabled" ||
    TRANSIENT.has(status.reason) ||
    status.reason === "composerEmpty"
  )
    return fallback;
  return status.hint;
}
