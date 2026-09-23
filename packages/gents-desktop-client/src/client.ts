import { normalizeInvokeError } from "./errors.js";
import { DesktopTransport, tauriTransport } from "./transport.js";
import { createDesktopApiAdapter } from "./api/adapter.js";
import type { DesktopApiAdapter } from "./api/types.js";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "./types.js";

/**
 * Typed command surface over an injected transport.
 * Commands mirror the bridge plugin (desktop_* names; transport adds plugin: prefix).
 */
export type DesktopClient = {
  transport: DesktopTransport;
  /** Full command API bound to this client's transport. */
  api: DesktopApiAdapter;
  invoke<T>(command: string, args?: unknown): Promise<T>;
  clientStart(): Promise<DesktopClientSnapshot>;
  clientShutdown(): Promise<DesktopClientSnapshot>;
  clientSnapshot(): Promise<DesktopClientSnapshot>;
  chatSend(request: ChatSendRequest): Promise<ChatSendResult>;
  sessionSnapshot(args: {
    sessionId: string;
    agentDid?: string | null;
    requestId?: string | null;
  }): Promise<DesktopSessionSnapshot | null>;
};

export function createDesktopClient(
  transport: DesktopTransport = tauriTransport(),
): DesktopClient {
  const rawApi = createDesktopApiAdapter(transport);

  async function invoke<T>(command: string, args?: unknown): Promise<T> {
    try {
      return await transport.invoke<T>(command, args);
    } catch (error) {
      throw normalizeInvokeError(error);
    }
  }

  async function clientStart(): Promise<DesktopClientSnapshot> {
    return invoke("desktop_client_start");
  }

  const api: DesktopApiAdapter = {
    ...rawApi,
    startDesktopClient: clientStart,
  };

  return {
    transport,
    api,
    invoke,
    clientStart,
    clientShutdown: () => invoke("desktop_client_shutdown"),
    clientSnapshot: () => invoke("desktop_client_snapshot"),
    chatSend: (request) => invoke("desktop_chat_send", { request }),
    sessionSnapshot: (args) =>
      invoke("desktop_session_snapshot", {
        sessionId: args.sessionId,
        agentDid: args.agentDid ?? null,
        requestId: args.requestId ?? null,
      }),
  };
}
