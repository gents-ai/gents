import { describe, expect, it } from "vitest";

import {
  asBridgeErrorPayload,
  BridgeInvokeError,
  normalizeInvokeError,
  packInstallRefusal,
} from "./errors.js";

describe("confirmed Goal-control rejection", () => {
  it.each(["storageFull", "runtimeKeyAccess"])(
    "preserves %s across host normalization",
    (code) => {
      const payload = {
        code,
        message: "Native recovery is required.",
        retryable: false,
        endpoint: null,
        operationCode: null,
        nextCall: null,
      };
      for (const wire of [payload, { message: payload }]) {
        const error = normalizeInvokeError(wire);
        expect(error).toBeInstanceOf(BridgeInvokeError);
        expect(asBridgeErrorPayload(error)).toEqual(payload);
      }
    },
  );
  it("preserves the native code and exact reason through host and Tauri errors", () => {
    const payload = {
      code: "goalControlRejected",
      message: "wait for this session to finish before starting a goal",
      retryable: false,
      endpoint: null,
      operationCode: null,
      nextCall: null,
    };
    for (const wire of [payload, { message: payload }]) {
      const error = normalizeInvokeError(wire);
      expect(error).toBeInstanceOf(BridgeInvokeError);
      expect(asBridgeErrorPayload(error)).toEqual(payload);
    }
  });

  it("keeps transport and message-only lookalikes distinct", () => {
    const message =
      "Runtime rejected goal control: wait for this session to finish before starting a goal";
    const untyped = new Error(message);
    expect(normalizeInvokeError(untyped)).toBe(untyped);
    expect(asBridgeErrorPayload(untyped)).toBeNull();
    const transport = normalizeInvokeError({
      code: "endpointUnreachable",
      message,
      retryable: true,
      endpoint: "http://127.0.0.1:1",
      operationCode: null,
      nextCall: null,
    });
    expect(asBridgeErrorPayload(transport)?.code).toBe("endpointUnreachable");
  });
});

describe("a typed pack install refusal", () => {
  const refusal = {
    class: "documents_edited",
    coordinate: "fixture/root",
    named: ["Tools/literal-tools"],
    unnamed: 2,
    sentence: "3 of fixture/root's documents were edited since it was installed.",
  };
  const payload = {
    code: "unknown",
    message: refusal.sentence,
    retryable: false,
    endpoint: null,
    operationCode: null,
    nextCall: null,
    packRefusal: refusal,
  };

  it("survives host normalization as typed fields", () => {
    const error = normalizeInvokeError(payload);
    expect(error).toBeInstanceOf(BridgeInvokeError);
    expect((error as BridgeInvokeError).packRefusal).toEqual(refusal);
    expect(packInstallRefusal(error)).toEqual(refusal);
  });

  it("is never read from an error's text or from an unknown class", () => {
    expect(packInstallRefusal(new Error("--overwrite documents_edited"))).toBeNull();
    expect(
      packInstallRefusal({ packRefusal: { ...refusal, class: "something_else" } }),
    ).toBeNull();
    expect(packInstallRefusal({ packRefusal: { ...refusal, unnamed: -1 } })).toBeNull();
  });
});
