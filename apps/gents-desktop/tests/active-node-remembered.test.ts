import { afterEach, describe, expect, it } from "vitest";

import { testApp, node } from "./app-fixture";
import { bootstrap } from "./config-panel-wiring/fixtures";

const remote = node({
  nodeDid: "did:key:remote",
  source: "enrollment",
  label: "Atlas",
});
const home = node({ nodeDid: "did:key:home", label: "Forge" });
const other = node({ nodeDid: "did:key:other", source: "enrollment", label: "Juno" });

/* the remote node is listed first, as an enrolled peer can be */
const start = () =>
  testApp({
    snapshot: {
      bootstrap: { ...bootstrap, initNodeDid: "did:key:home" },
      client: { clientDid: "client", deployments: [remote, home, other] },
    },
  });

const clear = () => {
  try {
    localStorage.removeItem("gents.activeNode");
  } catch {
    /* no storage */
  }
};

afterEach(clear);

describe("the active node across a reload", () => {
  it("opens on this machine's node when nothing was chosen", () => {
    clear();
    expect(start().stores.selection.getState().nodeDid).toBe("did:key:home");
  });

  it("opens on the node chosen last", () => {
    clear();
    const before = start();
    before.actions.selectNode("did:key:other");
    expect(start().stores.selection.getState().nodeDid).toBe("did:key:other");
  });

  it("falls back to this machine's node when the chosen one is gone", () => {
    try {
      localStorage.setItem("gents.activeNode", "did:key:removed");
    } catch {
      return;
    }
    expect(start().stores.selection.getState().nodeDid).toBe("did:key:home");
  });
});
