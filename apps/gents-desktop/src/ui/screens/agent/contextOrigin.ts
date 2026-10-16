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
