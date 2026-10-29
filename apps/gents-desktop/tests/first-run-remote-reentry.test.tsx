import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useFirstRun } from "../src/ui/app/useFirstRun";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { testApp, withApp } from "./app-fixture";

describe("remote-only desktop startup", () => {
  it("reopens a saved enrollment before its route is ready, including after restart", () => {
    const app = testApp({
      snapshot: {
        bootstrap: {
          ...bootstrap,
          initConfigStatus: "absent",
          initNodeDid: null,
          savedPeers: [{ ...deployment, source: "enrollment" }],
        },
        client: {
          deployments: [{ ...deployment, source: "enrollment", chatSafe: false }],
        },
      },
    });
    const { result, rerender } = renderHook(
      ({ generation, ready }) => useFirstRun(generation, ready),
      {
        initialProps: { generation: 1, ready: false },
        wrapper: withApp(app),
      },
    );
    expect(result.current.phase).toBe("unknown");
    rerender({ generation: 1, ready: true });
    expect(result.current.phase).toBe("done");
    rerender({ generation: 2, ready: true });
    expect(result.current.phase).toBe("done");
  });
});
