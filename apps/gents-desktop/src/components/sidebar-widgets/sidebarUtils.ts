import type { SessionSummary } from "@source-inc/gents-desktop-client";

export type SessionLifecycleGroup = "attention" | "active" | "recent";

export function sessionLifecycleGroup(session: SessionSummary): SessionLifecycleGroup {
  const state = (session.turnState ?? "").toLowerCase();
  if (
    [
      "failed",
      "error",
      "cancelled",
      "dead",
      "inputrequired",
      "input_required",
    ].includes(state)
  ) {
    return "attention";
  }
  if (state && !["completed", "superseded", "interrupted", "idle"].includes(state)) {
    return "active";
  }
  return "recent";
}

export function sessionStatusClass(session: SessionSummary) {
  const group = sessionLifecycleGroup(session);
  if (group === "attention") {
    return "session-status-dot session-status-dot-error";
  }
  return group === "active" ? "session-status-dot session-status-dot-running" : null;
}
