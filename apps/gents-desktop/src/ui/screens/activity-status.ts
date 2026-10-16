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

/** A stop applies only to the request it was made for, while it is in
 * flight: once its interrupt is accepted, or its own outcome carries it. */
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
