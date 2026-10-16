import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

export function activityStatus(
  items: readonly RenderedTimelineItem[],
  stopping: boolean,
): string | null {
  if (stopping) return "Stopping…";
  const last = items[items.length - 1];
  if (last?.kind === "toolGroup") {
    if (last.tools.some((tool) => tool.statusKind === "running")) return null;
    return "Reviewing results";
  }
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
