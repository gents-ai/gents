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
        endpoint: null,
      }),
    ).toEqual({
      code: "backend",
      message: "query failed",
      retryable: true,
      endpoint: null,
    });
  });

  it("rejects the pre-6.3 payload when endpoint is absent", () => {
    expect(
      asBridgeErrorPayload({
        code: "backend",
        message: "query failed",
        retryable: true,
      }),
    ).toBeNull();
  });

  it("carries the structured endpoint for endpointUnreachable payloads", () => {
    expect(
      asBridgeErrorPayload({
        code: "endpointUnreachable",
        message: "sending GET request to http://127.0.0.1:9181/status",
        retryable: true,
        endpoint: "http://127.0.0.1:9181",
      }),
    ).toEqual({
      code: "endpointUnreachable",
      message: "sending GET request to http://127.0.0.1:9181/status",
      retryable: true,
      endpoint: "http://127.0.0.1:9181",
    });
  });
});
