import type { SessionSummary } from "@source-inc/gents-desktop-client";

export function sessionBelongsToAgent(
  session: SessionSummary,
  selectedAgentDid: string | null,
) {
  if (!selectedAgentDid) {
    return true;
  }
  return session.agentDid === selectedAgentDid;
}
