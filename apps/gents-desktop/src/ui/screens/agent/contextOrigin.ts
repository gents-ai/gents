/* Which behavior a context page was opened from, so its Back returns there.
   Routes carry no origin, so the behavior that linked here is kept for the
   session; opening the Contexts list itself forgets it. PROPOSED screen. */
const origins = new Map<string, string>();

export function rememberContextOrigin(contextId: string, behaviorId: string) {
  origins.set(contextId, behaviorId);
}

export function forgetContextOrigins() {
  origins.clear();
}

export function contextOrigin(contextId: string) {
  return origins.get(contextId) ?? null;
}
