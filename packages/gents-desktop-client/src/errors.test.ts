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

describe("typed pack install refusals", () => {
  it("preserves fields through Tauri normalization without reading error text", () => {
    const packRefusal = {
      class: "documents_edited",
      coordinate: "acme/docs",
      named: ["Tools/literal"],
      unnamed: 2,
      sentence: "Desktop-specific sentence.",
    };
    const payload = {
      code: "unknown",
      message: "unrelated --overwrite lookalike",
      retryable: false,
      endpoint: null,
      packRefusal,
    };
    const error = normalizeInvokeError(payload);
    expect(packInstallRefusal(error)).toEqual(packRefusal);
    expect(asBridgeErrorPayload(error)?.packRefusal).toEqual(packRefusal);
    expect(packInstallRefusal(new Error(payload.message))).toBeNull();
    expect(
      packInstallRefusal({ packRefusal: { ...packRefusal, unnamed: -1 } }),
    ).toBeNull();
  });
});
