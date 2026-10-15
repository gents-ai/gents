import type { SessionSummary } from "@source-inc/gents-desktop-client";

export function sessionBelongsToAgent(
  session: SessionSummary,
  selectedAgentId: string | null,
) {
  if (!selectedAgentId) {
    return true;
  }
  return session.agentId === selectedAgentId;
}
