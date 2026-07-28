import { describe, expect, it } from "vitest";

import { formatPeerConnectionError } from "./peerConnectionErrors.js";

describe("formatPeerConnectionError", () => {
  it("turns local runtime transport context into operator-facing copy", () => {
    expect(
      formatPeerConnectionError(
        "sending GET request to http://127.0.0.1:9191/api/v0/p2p/shareable-address",
        "local-runtime",
      ),
    ).toBe(
      "Could not reach the local Gents runtime at http://127.0.0.1:9191. Start `gents server` and try again.",
    );
  });

  it("turns peer status transport context into discovery copy", () => {
    expect(
      formatPeerConnectionError(
        new Error(
          "sending GET request to http://127.0.0.1:9181/api/v0/p2p/shareable-address.",
        ),
        "peer-status",
      ),
    ).toBe(
      "Could not fetch runtime connection details from http://127.0.0.1:9181. Check that the runtime is running and the address is reachable.",
    );
  });

  it("keeps already useful errors unchanged", () => {
    expect(
      formatPeerConnectionError(
        "no running local Gents runtime found at /tmp/runtime.json; run `gents server` first",
        "local-runtime",
      ),
    ).toBe(
      "no running local Gents runtime found at /tmp/runtime.json; run `gents server` first",
    );
  });
});
