import type { ChatBlockedReason, SendStatus } from "@source-inc/gents-desktop-chat";

/* Reasons that pass on their own — the request being written, the runtime
   catching up — belong to the status line under the composer, which says
   "Sending…" or "Syncing…". The placeholder carries only a reason the
   person can act on; otherwise it stays the ordinary invitation. */
const TRANSIENT = new Set<ChatBlockedReason>([
  "submittingRequest",
  "waitingForRequestObservation",
  "inconsistentTurnObservation",
  "sessionMissingFromSnapshot",
]);

export function placeholderFor(status: SendStatus, fallback: string): string {
  if (status.kind !== "disabled" || TRANSIENT.has(status.reason)) return fallback;
  return status.hint;
}
