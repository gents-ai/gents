import { describe, expect, it } from "vitest";

import { formatPeerConnectionError } from "./peerConnectionErrors.js";

function endpointUnreachable(endpoint: string, message = "unused") {
  return {
    code: "endpointUnreachable" as const,
    message,
    retryable: true,
    endpoint,
  };
}

describe("formatPeerConnectionError", () => {
  it("turns a structured local-runtime endpoint into operator-facing copy", () => {
    expect(
      formatPeerConnectionError(
        endpointUnreachable("http://127.0.0.1:9191"),
        "local-runtime",
      ),
    ).toBe(
      "Could not reach the local Gents runtime at http://127.0.0.1:9191. Start `gents server` and try again.",
    );
  });

  it("turns a structured peer-status endpoint into discovery copy", () => {
    expect(
      formatPeerConnectionError(
        endpointUnreachable("http://127.0.0.1:9181"),
        "peer-status",
      ),
    ).toBe(
      "Could not fetch runtime connection details from http://127.0.0.1:9181. Check that the runtime is running and the address is reachable.",
    );
  });

  it("lets a white-label host own runtime and CLI names", () => {
    expect(
      formatPeerConnectionError(
        endpointUnreachable("http://127.0.0.1:9191"),
        "local-runtime",
        {
          runtimeProductName: "Indigo Relay",
          cliBinaryName: "indigo",
        },
      ),
    ).toBe(
      "Could not reach the local Indigo Relay runtime at http://127.0.0.1:9191. Start `indigo server` and try again.",
    );
  });

  it("recognizes the structured error nested under a Tauri invoke wrapper's message field", () => {
    expect(
      formatPeerConnectionError(
        { message: endpointUnreachable("http://127.0.0.1:9181") },
        "peer-status",
      ),
    ).toBe(
      "Could not fetch runtime connection details from http://127.0.0.1:9181. Check that the runtime is running and the address is reachable.",
    );
  });

  it("keeps already useful errors unchanged when there's no structured endpoint", () => {
    expect(
      formatPeerConnectionError(
        "no running local Gents runtime found at /tmp/runtime.json; run `gents server` first",
        "local-runtime",
      ),
    ).toBe(
      "no running local Gents runtime found at /tmp/runtime.json; run `gents server` first",
    );
  });

  it("leaves non-endpointUnreachable bridge errors unchanged", () => {
    expect(
      formatPeerConnectionError(
        {
          code: "backend",
          message: "GraphQL mutation failed",
          retryable: true,
          endpoint: null,
        },
        "peer-status",
      ),
    ).toBe("GraphQL mutation failed");
  });
});
