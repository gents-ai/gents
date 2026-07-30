import { normalizeInvokeError } from "./errors.js";
import { DesktopTransport, tauriTransport } from "./transport.js";
import { createDesktopApiAdapter } from "./api/adapter.js";
import type { DesktopApiAdapter } from "./api/types.js";
import type { BridgeContract as GeneratedBridgeContract } from "./generated/BridgeContract.js";
import type {
  BearerPairingRequest,
  BearerPairingResponse,
  ChatSendRequest,
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  PeerAddRequest,
} from "./types.js";

export type DesktopBridgeContract = GeneratedBridgeContract;

export const PACKAGE_VERSION = "0.10.0";
export const MINIMUM_BRIDGE_CONTRACT_VERSION = "0.5";

export function assertCompatibleBridgeContract(
  contract: DesktopBridgeContract,
) {
  const [requiredMajor, requiredMinor] =
    MINIMUM_BRIDGE_CONTRACT_VERSION.split(".").map(Number);
  const [actualMajor, actualMinor] = contract.contractVersion
    .split(".")
    .map(Number);
  if (
    !Number.isInteger(actualMajor) ||
    !Number.isInteger(actualMinor) ||
    actualMajor !== requiredMajor ||
    actualMinor < requiredMinor
  ) {
    throw new Error(
      `Incompatible Gents desktop bridge contract ${contract.contractVersion}; ` +
        `client requires ${requiredMajor}.${requiredMinor} or a newer compatible minor`,
    );
  }
  if (contract.packageVersion !== PACKAGE_VERSION) {
    throw new Error(
      `Gents desktop package mismatch: bridge ${contract.packageVersion}, client ${PACKAGE_VERSION}`,
    );
  }
}

export type DesktopClient = {
  transport: DesktopTransport;
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
  peerPairBearer(request: BearerPairingRequest): Promise<BearerPairingResponse>;
  peerAdd(request: PeerAddRequest): Promise<DesktopClientSnapshot>;
};

export function createDesktopClient(
  transport: DesktopTransport = tauriTransport(),
): DesktopClient {
  const api = createDesktopApiAdapter(transport);

  async function invoke<T>(command: string, args?: unknown): Promise<T> {
    try {
      return await transport.invoke<T>(command, args);
    } catch (error) {
      throw normalizeInvokeError(error);
    }
  }

  return {
    transport,
    api,
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
    peerPairBearer: (request) =>
      invoke("desktop_peer_pair_bearer", { request }),
    peerAdd: (request) => invoke("desktop_peer_add", { request }),
  };
}
