import type { SessionSummary } from "@source-inc/gents-desktop-client";

export function sessionBelongsToBehavior(
  session: SessionSummary,
  selectedBehaviorId: string | null,
) {
  if (!selectedBehaviorId) {
    return true;
  }
  return session.behaviorId === selectedBehaviorId;
}
