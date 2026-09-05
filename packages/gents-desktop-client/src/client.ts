import { normalizeInvokeError } from "./errors.js";
import { DesktopTransport, tauriTransport } from "./transport.js";
import { createDesktopApiAdapter } from "./api/adapter.js";
import type { DesktopApiAdapter } from "./api/types.js";
import type { BridgeContract as GeneratedBridgeContract } from "./generated/BridgeContract.js";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "./types.js";

export type DesktopBridgeContract = GeneratedBridgeContract;

export const PACKAGE_VERSION = "0.15.0";
// The client and bridge share one exact breaking contract. Sync status comes
// from database-owned gauges; goal permissions are explicit fields.
export const BRIDGE_CONTRACT_VERSION = "6.3";
export const EXPECTED_BRIDGE_WIRE_SCHEMA_HASH =
  "56091dd558796e1dd812bb794a134de193fee7706e81aa33082f53142232f47e";

export function assertExactBridgeContract(
  contract: DesktopBridgeContract,
) {
  if (contract.contractVersion !== BRIDGE_CONTRACT_VERSION) {
    throw new Error(
      `Incompatible Gents desktop bridge contract ${contract.contractVersion}; ` +
        `client requires exactly ${BRIDGE_CONTRACT_VERSION}`,
    );
  }
  if (contract.packageVersion !== PACKAGE_VERSION) {
    throw new Error(
      `Gents desktop package mismatch: bridge ${contract.packageVersion}, client ${PACKAGE_VERSION}`,
    );
  }
  if (contract.wireSchemaHash !== EXPECTED_BRIDGE_WIRE_SCHEMA_HASH) {
    throw new Error(
      `Incompatible Gents desktop wire schema ${contract.wireSchemaHash}; ` +
        `client requires ${EXPECTED_BRIDGE_WIRE_SCHEMA_HASH}`,
    );
  }
}

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
  bridgeContract(): Promise<DesktopBridgeContract>;
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
    assertExactBridgeContract(
      await invoke<DesktopBridgeContract>("desktop_bridge_contract"),
    );
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
    bridgeContract: () => invoke("desktop_bridge_contract"),
    chatSend: (request) => invoke("desktop_chat_send", { request }),
    sessionSnapshot: (args) =>
      invoke("desktop_session_snapshot", {
        sessionId: args.sessionId,
        agentDid: args.agentDid ?? null,
        requestId: args.requestId ?? null,
      }),
  };
}
