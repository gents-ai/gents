import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

/* What the run is doing now, read from the tail of the transcript. A step
   that is running already shows its own loader, so it has no line here. */
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
