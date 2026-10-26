import { describe, expect, it } from "vitest";

import {
  asBridgeErrorPayload,
  BridgeInvokeError,
  normalizeInvokeError,
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
    });
    expect(asBridgeErrorPayload(transport)?.code).toBe("endpointUnreachable");
  });
});
