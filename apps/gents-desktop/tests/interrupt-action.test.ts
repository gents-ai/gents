import { describe, expect, it, vi } from "vitest";

import { wasShown } from "../src/hooks/actionFailure";
import { testApp } from "./app-fixture";

describe("stopping a request", () => {
  it("asks the bridge to interrupt that request, as the person's cancel", async () => {
    const interruptRequest = vi.fn().mockResolvedValue({});
    const app = testApp({ api: { interruptRequest } });
    await app.actions.interruptRequest({ requestId: "req-1", nodeDid: "did:key:a" });
    expect(interruptRequest).toHaveBeenCalledWith({
      requestId: "req-1",
      nodeDid: "did:key:a",
      cause: "userCancelled",
    });
  });

  it("reports a failure once and rethrows it as shown", async () => {
    const reportFailure = vi.fn();
    const app = testApp({
      api: { interruptRequest: vi.fn().mockRejectedValue(new Error("gone")) },
      reportFailure,
    });
    const failure = await app.actions
      .interruptRequest({ requestId: "req-1", nodeDid: "did:key:a" })
      .catch((e: unknown) => e);
    expect(reportFailure).toHaveBeenCalledExactlyOnceWith("Couldn’t stop: gone");
    expect(wasShown(failure)).toBe(true);
  });
});
