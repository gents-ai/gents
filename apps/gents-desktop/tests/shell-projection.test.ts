import { describe, expect, it } from "vitest";

import { createChatStore } from "../src/hooks/chatStore";
import { createClientStore } from "../src/hooks/clientStore";
import { createFleetStore } from "../src/hooks/fleetStore";
import { createSelectionStore } from "../src/hooks/selectionStore";
import { createSessionStore } from "../src/hooks/sessionStore";
import {
  projectShell,
  projectionInputsOf,
  type ShellStores,
} from "../src/hooks/shellProjection";

/* the projection an action reads is the stores' state when it runs, with
   no copy kept in step during render */
describe("the shell projection from the stores", () => {
  it("tracks the request a send is awaiting, as the stores hold it now", () => {
    const stores: ShellStores = {
      selection: createSelectionStore({ agentDid: "node", sessionId: "s1" }),
      session: createSessionStore(),
      fleet: createFleetStore(),
      client: createClientStore(),
      chat: createChatStore(),
    };
    const now = () => projectShell(projectionInputsOf(stores)).trackedRequestId;
    expect(now()).toBeNull();
    stores.chat.setState({
      localWorkflow: {
        kind: "awaitingObservation",
        agentDid: "node",
        sessionId: "s1",
        requestId: "r1",
      },
    });
    expect(now()).toBe("r1");
    /* another session selected: its own workflow is not this one's */
    stores.selection.setState({ sessionId: "s2" });
    expect(now()).toBeNull();
  });
});
