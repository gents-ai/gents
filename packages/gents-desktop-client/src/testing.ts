import type { DesktopTransport, ClientUpdateEvent, Unlisten } from "./transport.js";

export type MemoryTransportOptions = {
  handlers?: Record<string, (args?: unknown) => unknown | Promise<unknown>>;
};

/**
 * Deterministic in-memory transport for harnesses and package tests.
 * Replaces setDesktopApiAdapterForTests with constructor injection.
 */
export function createMemoryTransport(
  options: MemoryTransportOptions = {},
): DesktopTransport & {
  emitClientUpdated(event?: ClientUpdateEvent): void;
  calls: Array<{ command: string; args?: unknown }>;
} {
  const handlers = options.handlers ?? {};
  const calls: Array<{ command: string; args?: unknown }> = [];
  const updateListeners = new Set<(e: ClientUpdateEvent) => void>();

  return {
    calls,
    async invoke<T>(command: string, args?: unknown): Promise<T> {
      calls.push({ command, args });
      const bare = command.includes("|") ? command.split("|")[1]! : command;
      const handler = handlers[bare] ?? handlers[command];
      if (!handler) {
        throw new Error(`memory transport: no handler for ${command}`);
      }
      return (await handler(args)) as T;
    },
    async listenClientUpdated(handler) {
      updateListeners.add(handler);
      const unlisten: Unlisten = () => {
        updateListeners.delete(handler);
      };
      return unlisten;
    },
    emitClientUpdated(event = { reason: "store" }) {
      for (const listener of updateListeners) {
        listener(event);
      }
    },
  };
}
