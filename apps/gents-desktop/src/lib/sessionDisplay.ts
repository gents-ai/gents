/** Display helpers for canonical AgentSession documents. */
export function displaySessionTitle(value?: string | null): string {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : "untitled";
}
