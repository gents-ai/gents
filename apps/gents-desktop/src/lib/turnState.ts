/* A session's turn state, as the bridge reports it (turn_state_label):
   waitingForClaim and streaming are live; completed, failed, superseded
   and interrupted are settled; null means no turn yet. The set is the
   desktop's conversation_is_active, so an unknown state counts as live. */
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
