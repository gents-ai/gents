import type { BridgeError as GeneratedBridgeError } from "./generated/BridgeError.js";
import type { BridgeErrorCode } from "./generated/BridgeErrorCode.js";

export type BridgeErrorPayload = GeneratedBridgeError;

const BRIDGE_ERROR_CODES = new Set<BridgeErrorCode>([
  "clientNotRunning",
  "clientStartFailed",
  "notFound",
  "invalidArgument",
  "unsupported",
  "endpointUnreachable",
  "stalePreview",
  "cascadeDepthExceeded",
  "pathEscapesRoot",
  "backend",
  "pairing",
  "unknown",
]);

export class BridgeInvokeError extends Error {
  readonly code: BridgeErrorCode;
  readonly retryable: boolean;
  /** The unreachable endpoint, for `code === "endpointUnreachable"`. */
  readonly endpoint: string | null;

  constructor(payload: BridgeErrorPayload) {
    super(payload.message);
    this.name = "BridgeInvokeError";
    this.code = payload.code;
    this.retryable = payload.retryable;
    this.endpoint = payload.endpoint ?? null;
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
    BRIDGE_ERROR_CODES.has(candidate.code as BridgeErrorCode) &&
    typeof candidate.message === "string" &&
    typeof candidate.retryable === "boolean"
  ) {
    return {
      code: candidate.code as BridgeErrorCode,
      message: candidate.message,
      retryable: candidate.retryable,
      endpoint: typeof candidate.endpoint === "string" ? candidate.endpoint : null,
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
