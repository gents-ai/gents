/** Injected transport and the package's only Tauri API import boundary. */

import type { ClientUpdateEvent as GeneratedClientUpdateEvent } from "./generated/ClientUpdateEvent.js";

export type ClientUpdateEvent = Partial<GeneratedClientUpdateEvent>;

export type Unlisten = () => void;

/* The bridge opens the browser for a provider's sign-in itself; it also
   sends the URL on these events for the person's "Open browser" fallback. */
const PROVIDER_LOGIN_URL_EVENT = {
  openai: "desktop://codex-login-url",
  anthropic: "desktop://claude-login-url",
  grok: "desktop://grok-login-url",
} as const;

export type OauthProvider = keyof typeof PROVIDER_LOGIN_URL_EVENT;

export interface DesktopTransport {
  invoke<T>(command: string, args?: unknown): Promise<T>;
  listenClientUpdated(
    handler: (e: ClientUpdateEvent) => void,
  ): Promise<Unlisten>;
  listenProviderLoginUrl?(
    provider: OauthProvider,
    handler: (url: string) => void,
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
    async listenProviderLoginUrl(provider, handler) {
      if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
        return () => {};
      }
      const { listen } = await import("@tauri-apps/api/event");
      return listen<{ url?: string }>(
        PROVIDER_LOGIN_URL_EVENT[provider],
        (event) => {
          const url = event.payload?.url;
          if (url) handler(url);
        },
      );
    },
  };
}
