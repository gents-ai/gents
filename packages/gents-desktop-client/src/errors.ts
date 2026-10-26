import type { BridgeError as GeneratedBridgeError } from "./generated/BridgeError.js";
import type { BridgeErrorCode } from "./generated/BridgeErrorCode.js";

export type BridgeErrorPayload = GeneratedBridgeError;

const BRIDGE_ERROR_CODES: Record<BridgeErrorCode, true> = {
  clientNotRunning: true,
  clientStartFailed: true,
  localHistoryLocked: true,
  runtimeKeyAccess: true,
  storageFull: true,
  notFound: true,
  projectionPending: true,
  goalControlRejected: true,
  invalidArgument: true,
  unsupported: true,
  endpointUnreachable: true,
  pathEscapesRoot: true,
  backend: true,
  incompatibleLocalStore: true,
  pairing: true,
  credentialNotSaved: true,
  runtimeStillBooting: true,
  sessionNotOwned: true,
  unknown: true,
};

export class BridgeInvokeError extends Error {
  readonly code: BridgeErrorCode;
  readonly retryable: boolean;
  readonly endpoint: string | null;

  constructor(payload: BridgeErrorPayload) {
    super(payload.message);
    this.name = "BridgeInvokeError";
    this.code = payload.code;
    this.retryable = payload.retryable;
    this.endpoint = payload.endpoint;
  }
}

export function asBridgeErrorPayload(
  error: unknown,
): BridgeErrorPayload | null {
  if (!error || typeof error !== "object") {
    return null;
  }
  const record = error as Record<string, unknown>;
  const candidate =
    typeof record.message === "object" && record.message !== null
      ? (record.message as Record<string, unknown>)
      : record;
  if (
    typeof candidate.code === "string" &&
    Object.hasOwn(BRIDGE_ERROR_CODES, candidate.code) &&
    typeof candidate.message === "string" &&
    typeof candidate.retryable === "boolean" &&
    (typeof candidate.endpoint === "string" || candidate.endpoint === null)
  ) {
    return {
      code: candidate.code as BridgeErrorCode,
      message: candidate.message,
      retryable: candidate.retryable,
      endpoint: candidate.endpoint as string | null,
    };
  }
  return null;
}

export function normalizeInvokeError(error: unknown): Error {
  const payload = asBridgeErrorPayload(error);
  if (payload) {
    return new BridgeInvokeError(payload);
  }
  if (error instanceof Error) {
    return error;
  }
  return new Error(String(error));
}
