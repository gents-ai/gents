import type { ChatBlockedReason, SendStatus } from "@source-inc/gents-desktop-chat";

// Only a reason the person can act on is carried in the placeholder; a
// transient reason belongs to the status line under the composer.
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
