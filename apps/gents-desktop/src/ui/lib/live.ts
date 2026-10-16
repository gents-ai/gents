import { isLive } from "../../lib/turnState";

export { isLive };

export const turnLabel = (turnState: string | null | undefined): string | null => {
  switch (turnState) {
    case "waitingForClaim":
      return "Waiting for node";
    case "running":
      return "Working";
    case "failed":
      return "Failed";
    case "interrupted":
      return "Stopped";
    case "superseded":
      return "Superseded";
    default:
      return isLive(turnState) ? "Working" : null;
  }
};
