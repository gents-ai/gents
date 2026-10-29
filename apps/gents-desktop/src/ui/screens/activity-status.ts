import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import {
  isTerminalTurnState,
  type RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";

export function interruptVisible(kind: ChatWorkflowState["kind"]): boolean {
  return kind === "awaitingObservation" || kind === "turnInProgress";
}

export function interruptTarget({
  trackedRequestId,
  latestRequestId,
  turnState,
}: {
  trackedRequestId: string | null;
  latestRequestId: string | null;
  turnState: string | null;
}): string | null {
  if (latestRequestId && turnState && !isTerminalTurnState(turnState))
    return latestRequestId;
  return trackedRequestId;
}

export function activityStatus(
  items: readonly RenderedTimelineItem[],
  stopping: boolean,
  executionActivity?: string | null,
): string | null {
  if (stopping) return "Stopping…";
  const last = items[items.length - 1];
  if (last?.kind === "toolGroup") {
    if (last.tools.some((tool) => tool.statusKind === "running")) return null;
  }
  switch (executionActivity) {
    case "queued":
      return "Waiting to start";
    case "waiting":
      return "Waiting for tools";
    case "retrying":
      return "Retrying connection";
    case "unavailable":
      return "Provider unavailable";
    case "unknown":
      return "Waiting for an update";
    case "terminal":
      return null;
  }
  if (last?.kind === "toolGroup") return "Reviewing results";
  if (last?.kind === "liveAssistant" && last.content?.trim()) return "Writing";
  return "Thinking";
}

/** A stop applies only to its request while in flight, until its interrupt
 * is accepted or that request's outcome carries it. */
export function isStopping({
  inFlight,
  requestId,
  latestRequestId,
  interruptObserved,
  requestedStop,
}: {
  inFlight: boolean;
  requestId: string | null;
  latestRequestId: string | null;
  interruptObserved: boolean;
  requestedStop: string | null;
}): boolean {
  if (!inFlight || !requestId) return false;
  return (
    requestedStop === requestId || (latestRequestId === requestId && interruptObserved)
  );
}
