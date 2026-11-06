import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";
import { runtimeConnectionBlocksAdmission } from "@source-inc/gents-desktop-client";

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
    const cases = JSON.parse(stdout) as { status: string; blocksAdmission: boolean }[];
    expect(cases.length).toBeGreaterThan(0);
    for (const row of cases) {
      expect(runtimeConnectionBlocksAdmission(row.status)).toBe(row.blocksAdmission);
    }
  });
});
