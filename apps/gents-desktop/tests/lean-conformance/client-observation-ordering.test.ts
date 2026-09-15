import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

import { acceptsAsyncResult } from "../../src/hooks/desktopShellRuntime";
import {
  projectStartupPhaseAfterSnapshot,
  type DesktopStartupPhase,
} from "../../src/lib/loadingStatus";

type ObservationFenceCase = {
  current: number;
  captured: number;
  accepted: boolean;
};

const run = promisify(execFile);

describe("generated client observation ordering", () => {
  it("drives the production async-result fence with every Lean row", async () => {
    const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "../../../..");
    const proofsDir = join(repoRoot, "crates/gents/proofs");
    await run("lake", ["build", "Proofs.Conformance.ClientObservationOrdering"], {
      cwd: proofsDir,
      timeout: 240_000,
    });
    const { stdout } = await run(
      "lake",
      ["env", "lean", "--run", "Proofs/Conformance/ClientObservationOrdering.lean"],
      { cwd: proofsDir, encoding: "utf8", timeout: 240_000 },
    );
    const { fences: cases, startup } = JSON.parse(stdout.trim()) as {
      fences: ObservationFenceCase[];
      startup: {
        phase: DesktopStartupPhase;
        running: boolean;
        pristine: boolean;
        expected: DesktopStartupPhase;
      }[];
    };

    expect(cases).toHaveLength(25);
    for (const row of cases) {
      expect(acceptsAsyncResult(row.current, row.captured)).toBe(row.accepted);
    }
    expect(startup).toHaveLength(28);
    for (const row of startup) {
      expect(
        projectStartupPhaseAfterSnapshot(row.phase, row.running, row.pristine),
      ).toBe(row.expected);
    }
  }, 300_000);
});
