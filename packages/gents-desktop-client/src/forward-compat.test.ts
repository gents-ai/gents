import { describe, expect, it } from "vitest";
import { asBridgeErrorPayload } from "./errors.js";

describe("forward compatibility", () => {
  it("accepts unknown error codes without crashing", () => {
    const payload = asBridgeErrorPayload({
      code: "futureCodeFromNewerBridge",
      message: "something new",
      retryable: false,
    });
    expect(payload?.code).toBe("futureCodeFromNewerBridge");
  });

  it("tolerates extra optional fields on error payloads", () => {
    const payload = asBridgeErrorPayload({
      code: "unknown",
      message: "x",
      retryable: true,
      extraField: 42,
    });
    expect(payload?.retryable).toBe(true);
  });
});
