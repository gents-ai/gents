import type { RuntimePreparationOutcome } from "./generated/RuntimePreparationOutcome.js";
import { RUNTIME_PREPARATION_VERSION } from "./generated/RuntimePreparationProtocol.js";
import type { RuntimePreparationRefusal } from "./generated/RuntimePreparationRefusal.js";
import type { RuntimePreparationReport } from "./generated/RuntimePreparationReport.js";

const outcomes: Record<RuntimePreparationOutcome, true> = {
  started: true,
  replaced: true,
  reused: true,
};
const refusals: Record<RuntimePreparationRefusal, true> = {
  notInitialized: true,
  invalidHome: true,
  unsupported: true,
  unavailable: true,
  busy: true,
  starting: true,
  startupBusy: true,
  storeBusy: true,
  credentials: true,
  failed: true,
};

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function text(value: unknown): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error("Malformed runtime preparation receipt");
  }
  return value;
}

/** The native command owns service control; this decodes its private process receipt. */
export function decodeRuntimePreparationReport(
  value: unknown,
): RuntimePreparationReport {
  const report = record(value);
  if (report?.version !== RUNTIME_PREPARATION_VERSION) {
    throw new Error("Unsupported runtime preparation receipt version");
  }
  const result = record(report.result);
  if (
    result?.kind === "ready" &&
    typeof result.outcome === "string" &&
    Object.hasOwn(outcomes, result.outcome)
  ) {
    const build = text(result.build);
    if (!/^[0-9a-f]{64}$/.test(build)) {
      throw new Error("Malformed runtime preparation build");
    }
    return {
      version: RUNTIME_PREPARATION_VERSION,
      result: {
        kind: "ready",
        outcome: result.outcome as RuntimePreparationOutcome,
        home: text(result.home),
        node_did: text(result.node_did),
        graphql: text(result.graphql),
        build,
      },
    };
  }
  if (
    result?.kind === "refused" &&
    typeof result.reason === "string" &&
    Object.hasOwn(refusals, result.reason)
  ) {
    return {
      version: RUNTIME_PREPARATION_VERSION,
      result: {
        kind: "refused",
        reason: result.reason as RuntimePreparationRefusal,
        message: text(result.message),
      },
    };
  }
  throw new Error("Malformed runtime preparation receipt");
}
