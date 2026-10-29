export type ActivityExpirySession = {
  sessionId: string;
  nodeDid: string | null;
  latestRequestId: string | null;
  executionActivityExpiresAt?: number | null;
};

export function activityExpiry(
  session: ActivityExpirySession | null,
  selectedSession: string | null,
  selectedNode: string | null,
  now: number,
): { key: string; delay: number } | null {
  const expires = session?.executionActivityExpiresAt;
  if (
    !session ||
    session.sessionId !== selectedSession ||
    expires == null ||
    (selectedNode && session.nodeDid && session.nodeDid !== selectedNode)
  )
    return null;
  return {
    key: `${session.nodeDid}\0${session.sessionId}\0${session.latestRequestId}\0${expires}`,
    delay: Math.min(Math.max(0, expires - now), 2_147_483_647),
  };
}
