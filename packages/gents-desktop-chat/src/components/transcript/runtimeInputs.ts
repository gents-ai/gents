import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

export function isHiddenRuntimeInput(item: RenderedTimelineItem): boolean {
  if (item.kind === "automatedInput")
    return (
      item.origin.kind === "goalContinuation" ||
      (item.origin.kind === "backgroundCompletion" && !item.origin.notification)
    );
  if (item.kind === "pendingUserTurn")
    return (
      item.origin?.kind === "goalContinuation" ||
      item.origin?.kind === "backgroundCompletion"
    );
  return false;
}
