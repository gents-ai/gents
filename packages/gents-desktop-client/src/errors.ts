import type { BridgeError as GeneratedBridgeError } from "./generated/BridgeError.js";
import type { PackInstallRefusal } from "./generated/PackInstallRefusal.js";
import type { BridgeErrorCode } from "./generated/BridgeErrorCode.js";
import type { BridgeNextCall } from "./generated/BridgeNextCall.js";
import type { BridgeNextCallKind } from "./generated/BridgeNextCallKind.js";

type OperationFields = "operationCode" | "nextCall";

export type BridgeErrorPayload = Omit<GeneratedBridgeError, OperationFields> &
  Partial<Pick<GeneratedBridgeError, OperationFields>> & {
    packRefusal?: PackInstallRefusal | null;
  };

const PACK_REFUSAL_CLASSES: Record<PackInstallRefusal["class"], true> = {
  documents_edited: true,
  authority_required: true,
  digest_changed: true,
};

/** The typed refusal an install failed with, read from the error's own
    field, never from its text. */
export function packInstallRefusal(error: unknown): PackInstallRefusal | null {
  if (!error || typeof error !== "object") return null;
  const value = (error as { packRefusal?: unknown }).packRefusal;
  if (!value || typeof value !== "object") return null;
  const r = value as Record<string, unknown>;
  if (
    typeof r.class !== "string" ||
    !Object.hasOwn(PACK_REFUSAL_CLASSES, r.class) ||
    !(r.coordinate === null || typeof r.coordinate === "string") ||
    !Array.isArray(r.named) ||
    !r.named.every((name) => typeof name === "string") ||
    !Number.isSafeInteger(r.unnamed) ||
    (r.unnamed as number) < 0 ||
    typeof r.sentence !== "string"
  )
    return null;
  return value as PackInstallRefusal;
}

const NEXT_CALL_KINDS: Record<BridgeNextCallKind, true> = {
  verb: true,
  verbHelp: true,
  resourceHelp: true,
  inspect: true,
  list: true,
  preview: true,
};

function asNextCall(value: unknown): BridgeNextCall | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const record = value as Record<string, unknown>;
  if (
    typeof record.kind === "string" &&
    Object.hasOwn(NEXT_CALL_KINDS, record.kind) &&
    typeof record.resource === "string" &&
    (typeof record.verb === "string" || record.verb == null)
  ) {
    return {
      kind: record.kind as BridgeNextCallKind,
      resource: record.resource,
      verb: (record.verb as string | null | undefined) ?? null,
    };
  }
  return null;
}

const BRIDGE_ERROR_CODES: Record<BridgeErrorCode, true> = {
  clientNotRunning: true,
  clientStartFailed: true,
  localHistoryLocked: true,
  runtimeKeyAccess: true,
  storageFull: true,
  notFound: true,
  projectionPending: true,
  goalControlRejected: true,
  permissionDenied: true,
  invalidArgument: true,
  unsupported: true,
  endpointUnreachable: true,
  pathEscapesRoot: true,
  backend: true,
  incompatibleLocalStore: true,
  unfinishedLocalSetup: true,
  pairing: true,
  credentialNotSaved: true,
  runtimeStillBooting: true,
  sessionNotOwned: true,
  transactionConflict: true,
  updateRequired: true,
  unknown: true,
};

export class BridgeInvokeError extends Error {
  readonly packRefusal: PackInstallRefusal | null;
  readonly code: BridgeErrorCode;
  readonly retryable: boolean;
  readonly endpoint: string | null;
  readonly operationCode: string | null;
  readonly nextCall: BridgeNextCall | null;

  constructor(payload: BridgeErrorPayload) {
    super(payload.message);
    this.name = "BridgeInvokeError";
    this.packRefusal = packInstallRefusal(payload);
    this.code = payload.code;
    this.retryable = payload.retryable;
    this.endpoint = payload.endpoint;
    this.operationCode = payload.operationCode ?? null;
    this.nextCall = payload.nextCall ?? null;
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
    const packRefusal = packInstallRefusal(candidate);
    return {
      ...(packRefusal ? { packRefusal } : {}),
      code: candidate.code as BridgeErrorCode,
      message: candidate.message,
      retryable: candidate.retryable,
      endpoint: candidate.endpoint as string | null,
      operationCode:
        typeof candidate.operationCode === "string"
          ? candidate.operationCode
          : null,
      nextCall: asNextCall(candidate.nextCall),
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
