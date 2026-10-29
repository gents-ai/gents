import { describe, expect, it } from "vitest";
import { enrollmentActivationMessage } from "./enrollmentActivation.js";

describe("enrollment activation messages", () => {
  it("explains authentication and route failures only for approved requests", () => {
    expect(
      enrollmentActivationMessage({
        state: "approved",
        activationError: "authentication",
      }),
    ).toContain("authenticate the server");
    expect(
      enrollmentActivationMessage({
        state: "approved",
        activationError: "route",
      }),
    ).toContain("secure connection");
    expect(
      enrollmentActivationMessage({
        state: "pending_approval",
        activationError: "route",
      }),
    ).toBeNull();
    expect(enrollmentActivationMessage({ state: "approved", activationError: null })).toBeNull();
    expect(
      enrollmentActivationMessage({
        state: "approved",
        activationError: "unknown",
      }),
    ).toBeNull();
  });
});
