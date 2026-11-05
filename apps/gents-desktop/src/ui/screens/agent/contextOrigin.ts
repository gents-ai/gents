const origins = new Map<string, string>();

export function rememberContextOrigin(contextId: string, agentDid: string) {
  origins.set(contextId, agentDid);
}

export function forgetContextOrigins() {
  origins.clear();
}

export function contextOrigin(contextId: string) {
  return origins.get(contextId) ?? null;
}
