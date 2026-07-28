import type { BridgeError as GeneratedBridgeError } from "./generated/BridgeError.js";

/**
 * Known fields come from Rust. `code` remains open so a newer additive bridge
 * error can still be displayed by an older client.
 */
export type BridgeErrorPayload = Omit<GeneratedBridgeError, "code"> & {
  code: string;
};

export class BridgeInvokeError extends Error {
  readonly code: string;
  readonly retryable: boolean;

  constructor(payload: BridgeErrorPayload) {
    super(payload.message);
    this.name = "BridgeInvokeError";
    this.code = payload.code;
    this.retryable = payload.retryable;
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
    typeof candidate.message === "string" &&
    typeof candidate.retryable === "boolean"
  ) {
    return {
      code: candidate.code,
      message: candidate.message,
      retryable: candidate.retryable,
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
