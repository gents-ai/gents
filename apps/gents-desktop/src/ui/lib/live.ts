import { isLive } from "../../lib/turnState";

export { isLive };

/** A session's state: every turn state the summary carries falls in exactly
    one, and the row's glyph, its label and the state filter all read it. */
export type SessionState = "working" | "waiting" | "failed" | "stopped" | "finished";

export function stateOf(turnState: string | null | undefined): SessionState {
  if (turnState === "waitingForClaim") return "waiting";
  if (isLive(turnState)) return "working";
  switch (turnState?.toLowerCase()) {
    case "failed":
    case "error":
    case "dead":
      return "failed";
    case "interrupted":
    case "superseded":
    case "cancelled":
      return "stopped";
    default:
      return "finished";
  }
}

export const turnLabel = (turnState: string | null | undefined): string | null => {
  switch (stateOf(turnState)) {
    case "waiting":
      return "Waiting for node";
    case "working":
      return "Working";
    case "failed":
      return "Failed";
    case "stopped":
      return turnState?.toLowerCase() === "superseded" ? "Superseded" : "Stopped";
    default:
      return null;
  }
};
