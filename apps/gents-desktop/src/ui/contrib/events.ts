/* The plugin-facing tap on the desktop's update stream. The shell feeds
   every `desktop://client-updated` event through here; plugins subscribe
   by type (`'*'` for everything) via ctx.onEvent. Listeners are isolated:
   a throwing plugin never breaks the app's own handling, and emit costs
   nothing when nobody listens. After hermes-agent's contrib/events.ts. */
import type { DesktopClientUpdatedEvent } from "@source-inc/gents-desktop-client";

export interface UxEvent {
  /** `client-updated` today; `ux.<id>.<event>` once a plugin can emit */
  type: string;
  payload: unknown;
}

export type UxEventListener = (event: UxEvent) => void;

const listeners = new Map<string, Set<UxEventListener>>();

export function onUxEvent(type: string, listener: UxEventListener): () => void {
  const set = listeners.get(type) ?? new Set<UxEventListener>();
  set.add(listener);
  listeners.set(type, set);
  return () => {
    set.delete(listener);
    if (set.size === 0) listeners.delete(type);
  };
}

export function emitUxEvent(event: UxEvent): void {
  if (listeners.size === 0) return;
  for (const type of [event.type, "*"]) {
    for (const listener of listeners.get(type) ?? []) {
      try {
        listener(event);
      } catch (error) {
        console.error("[ux-plugins] event listener failed", error);
      }
    }
  }
}

/** the shell's one call: forward a client update into the plugin tap */
export function emitClientUpdated(payload: DesktopClientUpdatedEvent): void {
  emitUxEvent({ type: "client-updated", payload });
}

/** test seam */
export function listenerCount(type?: string): number {
  if (type) return listeners.get(type)?.size ?? 0;
  let n = 0;
  for (const set of listeners.values()) n += set.size;
  return n;
}
