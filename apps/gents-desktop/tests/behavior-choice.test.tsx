import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useBehaviorChoice } from "../src/ui/screens/SessionScreen";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

/* the choice on the new-session screen of the fixture node, whose default
   behavior is "default" and which also offers "ops" */
function choice() {
  const app = testApp({
    deployments: [node()],
    selection: { agentDid: deployment.agentDid },
  });
  const rendered = renderHook(() => useBehaviorChoice(), { wrapper: withApp(app) });
  return { app, result: rendered.result };
}

describe("new-session behavior choice", () => {
  it("shows only the selection's decision and never retains a shadow selection", () => {
    const { app, result } = choice();
    expect(result.current.behaviorId).toBe("default");
    act(() => result.current.setPicked("ops"));
    expect(app.stores.selection.getState().behaviorId).toBe("ops");
    expect(result.current.behaviorId).toBe("ops");
    act(() => app.actions.selectAgent("did:key:other"));
    expect(result.current.behaviorId).toBeNull();
  });

  it("forgets the behavior explicitly picked for the previous session", () => {
    const { app, result } = choice();
    act(() => result.current.setPicked("ops"));
    act(() => app.actions.selectSession("session-1"));
    act(() => app.actions.startNewSession());
    expect(result.current.behaviorId).toBe("default");
  });
});
