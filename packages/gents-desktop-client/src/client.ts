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
// Runtime-authored behavior readiness is required and the duplicate runtime
// counters are gone and status pairing is authenticated enrollment, so this
// client requires the breaking 4.0 bridge.
export const MINIMUM_BRIDGE_CONTRACT_VERSION = "4.0";
export const EXPECTED_BRIDGE_WIRE_SCHEMA_HASH =
  "7a0203eda69c7fedd3f50f89b48af64e7aac23c86a92718742dc9417d8c6bc7a";

function parseBridgeContractVersion(version: string): [number, number] | null {
  const match = /^(\d+)\.(\d+)$/.exec(version);
  if (!match) return null;
  const major = Number(match[1]);
  const minor = Number(match[2]);
  return Number.isSafeInteger(major) && Number.isSafeInteger(minor)
    ? [major, minor]
    : null;
}

export function assertCompatibleBridgeContract(
  contract: DesktopBridgeContract,
) {
  const required = parseBridgeContractVersion(MINIMUM_BRIDGE_CONTRACT_VERSION);
  const actual = parseBridgeContractVersion(contract.contractVersion);
  if (!required) {
    throw new Error(
      `Invalid desktop client bridge requirement ${MINIMUM_BRIDGE_CONTRACT_VERSION}`,
    );
  }
  const [requiredMajor, requiredMinor] = required;
  if (!actual || actual[0] !== requiredMajor || actual[1] < requiredMinor) {
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
    assertCompatibleBridgeContract(
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
