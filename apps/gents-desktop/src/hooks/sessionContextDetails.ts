import type {
  SessionContextDetails,
  SessionContextDetailsRequest,
} from "@source-inc/gents-desktop-client";

/** Call captures are immutable; missing replicas and failed reads may be retried on reopen. */
export function createContextDetailsCache(
  fetch: (
    request: SessionContextDetailsRequest,
  ) => Promise<SessionContextDetails | null>,
) {
  const entries = new Map<string, Promise<SessionContextDetails | null>>();
  return (request: SessionContextDetailsRequest) => {
    const key = JSON.stringify([
      request.nodeDid,
      request.sessionId,
      request.requestId,
      request.callId,
    ]);
    const existing = entries.get(key);
    if (existing) {
      entries.delete(key);
      entries.set(key, existing);
      return existing;
    }
    const discard = () => {
      if (entries.get(key) === pending) entries.delete(key);
    };
    const pending = Promise.resolve()
      .then(() => fetch(request))
      .then(
        (details) => {
          if (!details) discard();
          return details;
        },
        (error: unknown) => {
          discard();
          throw error;
        },
      );
    if (entries.size >= 64) entries.delete(entries.keys().next().value!);
    entries.set(key, pending);
    return pending;
  };
}
