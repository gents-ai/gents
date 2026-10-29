import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

import { acceptsLiveCursor } from "@source-inc/gents-desktop-client";
import { acceptsAsyncResult } from "../../src/hooks/observationOrdering";
import {
  projectStartupPhaseAfterSnapshot,
  type DesktopStartupPhase,
} from "../../src/lib/loadingStatus";
import { projectFirstRunPhase, type FirstRunPhase } from "../../src/ui/lib/firstRun";

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
    const {
      fences: cases,
      startup,
      firstRun,
      liveDeltas,
    } = JSON.parse(stdout.trim()) as {
      liveDeltas: {
        base: number | null;
        current: number | null;
        terminal: boolean;
        accepted: boolean;
      }[];
      fences: ObservationFenceCase[];
      startup: {
        phase: DesktopStartupPhase;
        running: boolean;
        autostartDeclined: boolean;
        expected: DesktopStartupPhase;
      }[];
      firstRun: {
        phase: FirstRunPhase;
        snapshotReady: boolean;
        initConfigAbsent: boolean;
        savedRemote: boolean;
        needsSetup: boolean;
        authoritative: boolean;
        expected: FirstRunPhase;
      }[];
    };

    expect(cases).toHaveLength(25);
    expect(liveDeltas).toHaveLength(18);
    for (const row of liveDeltas) {
      expect(
        acceptsLiveCursor(row.base?.toString(), row.current?.toString(), row.terminal),
      ).toBe(row.accepted);
    }
    for (const row of cases) {
      expect(acceptsAsyncResult(row.current, row.captured)).toBe(row.accepted);
    }
    expect(startup).toHaveLength(28);
    for (const row of startup) {
      expect(
        projectStartupPhaseAfterSnapshot(row.phase, row.running, row.autostartDeclined),
      ).toBe(row.expected);
    }
    expect(firstRun).toHaveLength(96);
    for (const row of firstRun) {
      expect(
        projectFirstRunPhase(
          row.phase,
          row.snapshotReady,
          row.initConfigAbsent ? "absent" : "configured",
          row.needsSetup,
          row.authoritative,
          row.savedRemote,
        ),
      ).toBe(row.expected);
    }
  }, 300_000);
});
