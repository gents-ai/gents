/* Which agent a context page was opened from, so its Back returns there.
   Routes carry no origin, so the agent that linked here is kept for the
   session; opening the Contexts list itself forgets it. */
const origins = new Map<string, string>();

export function rememberContextOrigin(contextId: string, agentId: string) {
  origins.set(contextId, agentId);
}

export function forgetContextOrigins() {
  origins.clear();
}

export function contextOrigin(contextId: string) {
  return origins.get(contextId) ?? null;
}
