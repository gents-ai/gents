import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";
import {
  projectRuntimeConnectionStatus,
  runtimeConnectionBlocksAdmission,
} from "@source-inc/gents-desktop-client";

const run = promisify(execFile);

describe("runtime connection admission", () => {
  it("matches the Lean observation owner", async () => {
    const proofsDir = join(
      dirname(fileURLToPath(import.meta.url)),
      "../../../../crates/gents/proofs",
    );
    await run("lake", ["build", "Proofs.Conformance.RuntimeConnectionObservation"], {
      cwd: proofsDir,
    });
    const { stdout } = await run(
      "lake",
      ["env", "lean", "--run", "Proofs/Conformance/RuntimeConnectionObservation.lean"],
      { cwd: proofsDir },
    );
    const contract = JSON.parse(stdout) as {
      cases: { status: string; blocksAdmission: boolean }[];
      presentations: { status: string | null; kind: string; reason: string }[];
    };
    expect(contract.cases.length).toBeGreaterThan(0);
    for (const row of contract.cases) {
      expect(runtimeConnectionBlocksAdmission(row.status)).toBe(row.blocksAdmission);
    }
    expect(contract.presentations.length).toBeGreaterThan(0);
    const labels = new Map<string, string>();
    for (const row of contract.presentations) {
      const shown = projectRuntimeConnectionStatus(row.status);
      expect({ status: row.status, kind: shown.kind, reason: shown.reason }).toEqual(row);
      const previous = labels.get(shown.label);
      expect(previous === undefined || previous === row.reason).toBe(true);
      labels.set(shown.label, row.reason);
    }
  });
});
