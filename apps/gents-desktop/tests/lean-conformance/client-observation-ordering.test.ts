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
import { projectStartupGate, type StartupGate } from "../../src/lib/startupGate";
import type { ManagedServerStatus } from "@source-inc/gents-desktop-client";

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
        initConfigStatus: "absent" | "configured" | "unreadable" | "unknown";
        localRuntimeAllowed: boolean;
        remoteConfigured: boolean;
        localHomeLost: boolean;
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
    expect(firstRun).toHaveLength(768);
    for (const row of firstRun) {
      expect(
        projectFirstRunPhase(
          row.phase,
          row.snapshotReady,
          row.initConfigStatus === "unknown" ? null : row.initConfigStatus,
          row.needsSetup,
          row.authoritative,
          row.localRuntimeAllowed,
          row.remoteConfigured,
          row.localHomeLost,
        ),
      ).toBe(row.expected);
    }
  }, 300_000);
});

const CONTRACT_JSON_BEGIN = "---BEGIN GENTS LEAN CONTRACT JSON---";
const CONTRACT_JSON_END = "---END GENTS LEAN CONTRACT JSON---";

type StartupGateCase = {
  input: {
    settled: boolean;
    init_config_status: "absent" | "configured" | "unreadable" | "unknown";
    node_not_connected: boolean;
  };
  expected: StartupGate;
};

describe("generated startup gate", () => {
  it("drives the startup gate with every case of its Lean family", async () => {
    const proofsDir = join(
      dirname(fileURLToPath(import.meta.url)),
      "../../../../crates/gents/proofs",
    );
    await run("lake", ["build", "Proofs.Conformance.Contracts"], {
      cwd: proofsDir,
      timeout: 240_000,
    });
    const { stdout } = await run(
      "lake",
      ["env", "lean", "--run", "Proofs/Conformance/Contracts.lean"],
      { cwd: proofsDir, encoding: "utf8", timeout: 240_000, maxBuffer: 1 << 30 },
    );
    const begin = stdout.indexOf(CONTRACT_JSON_BEGIN) + CONTRACT_JSON_BEGIN.length;
    const end = stdout.indexOf(CONTRACT_JSON_END);
    const { families } = JSON.parse(stdout.slice(begin, end)) as {
      families: Record<string, { cases: StartupGateCase[] }>;
    };
    const family = "families::client_snapshot_observation_startup_gate::cases()";
    const key = /^families::(\w+)::cases\(\)$/.exec(family)![1]!;
    const cases = families[key]!.cases;
    expect(cases).toHaveLength(16);
    /* a node the connect screen calls not connected is a stopped one this
       desktop is not enrolled with; otherwise there is no local node, or an
       enrolled one, each in turn */
    const stopped = { state: "stopped", pairingReady: false } as ManagedServerStatus;
    const enrolled = { state: "running", pairingReady: true } as ManagedServerStatus;
    let other = 0;
    for (const { input, expected } of cases) {
      const localNode = input.node_not_connected
        ? stopped
        : other++ % 2
          ? enrolled
          : null;
      expect(
        projectStartupGate(
          input.settled,
          input.init_config_status === "unknown" ? null : input.init_config_status,
          localNode,
        ),
      ).toBe(expected);
    }
  }, 300_000);
});
