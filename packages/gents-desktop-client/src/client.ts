import { normalizeInvokeError } from "./errors.js";
import { DesktopTransport, tauriTransport } from "./transport.js";

/**
 * Typed command surface over an injected transport.
 * Commands mirror the bridge plugin (desktop_* names; transport adds plugin: prefix).
 */
export type DesktopClient = {
  transport: DesktopTransport;
  invoke<T>(command: string, args?: unknown): Promise<T>;
  clientStart(): Promise<unknown>;
  clientShutdown(): Promise<unknown>;
  clientSnapshot(): Promise<unknown>;
  bridgeContract(): Promise<unknown>;
  chatSend(request: unknown): Promise<unknown>;
  sessionSnapshot(args: {
    sessionId: string;
    agentDid?: string | null;
    requestId?: string | null;
  }): Promise<unknown>;
  peerPairBearer(request: unknown): Promise<unknown>;
  peerAdd(request: unknown): Promise<unknown>;
};

export function createDesktopClient(transport: DesktopTransport = tauriTransport()): DesktopClient {
  async function invoke<T>(command: string, args?: unknown): Promise<T> {
    try {
      return await transport.invoke<T>(command, args);
    } catch (error) {
      throw normalizeInvokeError(error);
    }
  }

  return {
    transport,
    invoke,
    clientStart: () => invoke("desktop_client_start"),
    clientShutdown: () => invoke("desktop_client_shutdown"),
    clientSnapshot: () => invoke("desktop_client_snapshot"),
    bridgeContract: () => invoke("desktop_bridge_contract"),
    chatSend: (request) => invoke("desktop_chat_send", { request }),
    sessionSnapshot: (args) =>
      invoke("desktop_session_snapshot", {
        sessionId: args.sessionId,
        agentDid: args.agentDid ?? null,
        requestId: args.requestId ?? null,
      }),
    peerPairBearer: (request) => invoke("desktop_peer_pair_bearer", { request }),
    peerAdd: (request) => invoke("desktop_peer_add", { request }),
  };
}
