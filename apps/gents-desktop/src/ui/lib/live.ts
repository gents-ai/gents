import { isLive } from "../../lib/turnState";

export { isLive };

/* what to say next to a session: nothing for a finished one, and working
   for a live state this build does not name, as its spinner says */
export const turnLabel = (turnState: string | null | undefined): string | null => {
  switch (turnState) {
    case "waitingForClaim":
      return "Waiting for node";
    case "running":
      return "Working";
    case "failed":
      return "Failed";
    case "interrupted":
      return "Interrupted";
    case "superseded":
      return "Superseded";
    default:
      return isLive(turnState) ? "Working" : null;
  }
};
