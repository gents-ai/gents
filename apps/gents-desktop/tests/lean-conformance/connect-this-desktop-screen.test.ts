import { describe, expect, it } from "vitest";

import {
  connectScreen,
  type ConnectNodeState,
  type ConnectRefusal,
  type ConnectScreen,
} from "../../src/lib/connectScreen";
import { runLeanGeneratorJson } from "./leanGenerator";

describe("generated connect this desktop screen", () => {
  it("shows what Lean DesktopConnect.screen shows for every node state", async () => {
    const rows = await runLeanGeneratorJson<
      {
        node: ConnectNodeState;
        enrolled: boolean;
        screen: ConnectScreen;
        refusal: ConnectRefusal | null;
      }[]
    >(
      "Proofs.Conformance.ConnectThisDesktopScreen:olean",
      "Proofs/Conformance/ConnectThisDesktopScreen.lean",
    );
    expect(rows).toHaveLength(8);
    for (const row of rows) {
      expect(connectScreen(row.node, row.enrolled), JSON.stringify(row)).toEqual([
        row.screen,
        row.refusal,
      ]);
    }
  }, 600_000);
});
