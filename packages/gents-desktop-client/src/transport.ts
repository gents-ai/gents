/** Injected transport and the package's only Tauri API import boundary. */

import type { ClientUpdateEvent as GeneratedClientUpdateEvent } from "./generated/ClientUpdateEvent.js";

export type ClientUpdateEvent = Partial<GeneratedClientUpdateEvent>;

export type Unlisten = () => void;

export interface DesktopTransport {
  invoke<T>(command: string, args?: unknown): Promise<T>;
  listenClientUpdated(
    handler: (e: ClientUpdateEvent) => void,
  ): Promise<Unlisten>;
}

const BRIDGE_PLUGIN = "gents-desktop-bridge";

export function bridgeCommand(command: string): string {
  if (command.startsWith("plugin:")) {
    return command;
  }
  return `plugin:${BRIDGE_PLUGIN}|${command}`;
}

export function tauriTransport(): DesktopTransport {
  return {
    async invoke<T>(command: string, args?: unknown): Promise<T> {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<T>(
        bridgeCommand(command),
        args as Record<string, unknown> | undefined,
      );
    },
    async listenClientUpdated(handler) {
      const { listen } = await import("@tauri-apps/api/event");
      const unlisten = await listen<ClientUpdateEvent>(
        "desktop://client-updated",
        (event) => {
          handler(event.payload ?? {});
        },
      );
      return () => {
        unlisten();
      };
    },
  };
}
