import { describe, expect, it } from "vitest";
import { decodeRuntimePreparationReport } from "./runtimePreparation.js";
import { RUNTIME_PREPARATION_VERSION } from "./generated/RuntimePreparationProtocol.js";

describe("native runtime preparation receipts", () => {
  const ready = {
    version: RUNTIME_PREPARATION_VERSION,
    result: {
      kind: "ready",
      outcome: "replaced",
      home: "/runtime",
      node_did: "did:key:fixture",
      graphql: "http://127.0.0.1:9191/api/v0/graphql",
      build: "a".repeat(64),
    },
  };

  it("preserves native readiness and refusal outcomes", () => {
    expect(decodeRuntimePreparationReport(ready)).toEqual(ready);
    const refused = {
      version: RUNTIME_PREPARATION_VERSION,
      result: {
        kind: "refused",
        reason: "busy",
        message: "The runtime still owns active work.",
      },
    };
    expect(decodeRuntimePreparationReport(refused)).toEqual(refused);
  });

  it("rejects mismatched helpers and malformed success receipts", () => {
    for (const value of [
      { ...ready, version: 0 },
      { ...ready, result: { ...ready.result, outcome: "toString" } },
      { ...ready, result: { ...ready.result, build: "unknown" } },
      { ...ready, result: { ...ready.result, node_did: null } },
      { ...ready, result: { ...ready.result, graphql: "" } },
      { ...ready, result: { kind: "refused", reason: "retryAnything" } },
      { ...ready, result: null },
      null,
    ]) {
      expect(() => decodeRuntimePreparationReport(value)).toThrow();
    }
  });
});
