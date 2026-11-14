import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

import {
  connectScreen,
  type ConnectNodeState,
  type ConnectRefusal,
  type ConnectScreen,
} from "../../src/lib/connectScreen";

const run = promisify(execFile);

describe("generated connect this desktop screen", () => {
  it("shows what Lean DesktopConnect.screen shows for every node state", async () => {
    const proofsDir = join(
      dirname(fileURLToPath(import.meta.url)),
      "../../../../crates/gents/proofs",
    );
    await run("lake", ["build", "Proofs.Clients.DesktopConnect"], {
      cwd: proofsDir,
      timeout: 240_000,
    });
    const { stdout } = await run(
      "lake",
      ["env", "lean", "--run", "Proofs/Conformance/ConnectThisDesktopScreen.lean"],
      { cwd: proofsDir, encoding: "utf8", timeout: 240_000 },
    );
    const rows = JSON.parse(stdout.trim()) as {
      node: ConnectNodeState;
      enrolled: boolean;
      screen: ConnectScreen;
      refusal: ConnectRefusal | null;
    }[];
    expect(rows).toHaveLength(8);
    for (const row of rows) {
      expect(connectScreen(row.node, row.enrolled), JSON.stringify(row)).toEqual([
        row.screen,
        row.refusal,
      ]);
    }
  }, 600_000);
});
