import { describe, expect, it } from "vitest";
import { asBridgeErrorPayload } from "./errors.js";

describe("bridge error contract", () => {
  it("rejects error codes outside the exact bridge contract", () => {
    expect(
      asBridgeErrorPayload({
        code: "unrecognizedCode",
        message: "something new",
        retryable: false,
      }),
    ).toBeNull();
  });

  it("accepts an exact bridge error payload", () => {
    expect(
      asBridgeErrorPayload({
        code: "backend",
        message: "query failed",
        retryable: true,
      }),
    ).toEqual({ code: "backend", message: "query failed", retryable: true });
  });
});
