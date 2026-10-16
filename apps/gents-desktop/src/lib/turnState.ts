// Mirrors the bridge's turn_state_label set; unknown states count as live.
const SETTLED = new Set([
  "completed",
  "failed",
  "error",
  "dead",
  "superseded",
  "interrupted",
  "cancelled",
  "idle",
]);
export const isLive = (turnState: string | null | undefined) =>
  Boolean(turnState) && !SETTLED.has(turnState!.toLowerCase());
