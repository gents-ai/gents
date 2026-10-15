import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useAgentChoice } from "../src/ui/screens/SessionScreen";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

/* the choice on the new-session screen of the fixture node, whose default
   agent is "default" and which also offers "ops" */
function choice() {
  const app = testApp({
    deployments: [node()],
    selection: { nodeDid: deployment.nodeDid },
  });
  const rendered = renderHook(() => useAgentChoice(), { wrapper: withApp(app) });
  return { app, result: rendered.result };
}

describe("new-session agent choice", () => {
  it("shows only the selection's decision and never retains a shadow selection", () => {
    const { app, result } = choice();
    expect(result.current.agentId).toBe("default");
    act(() => result.current.setPicked("ops"));
    expect(app.stores.selection.getState().agentId).toBe("ops");
    expect(result.current.agentId).toBe("ops");
    act(() => app.actions.selectNode("did:key:other"));
    expect(result.current.agentId).toBeNull();
  });

  it("forgets the agent explicitly picked for the previous session", () => {
    const { app, result } = choice();
    act(() => result.current.setPicked("ops"));
    act(() => app.actions.selectSession("session-1"));
    act(() => app.actions.startNewSession());
    expect(result.current.agentId).toBe("default");
  });
});
