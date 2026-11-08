import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

import type {
  DeploymentOperationalState,
  OperationalStatus,
} from "@source-inc/gents-desktop-client";
import { waitingRereadDelay } from "../../src/hooks/sessionReads";
import {
  projectSessionLoadingStatus,
  selectionMatches,
  type SessionLoadState,
  type SessionLoadingStatus,
} from "../../src/lib/loadingStatus";

type Blocker = {
  layer: OperationalStatus["layer"];
  kind: OperationalStatus["kind"];
  label: string;
  detail: string;
  configureInference: boolean;
};

type StatusRow = {
  selected: boolean;
  sessionMatches: boolean;
  loadMatches: boolean;
  phase: SessionLoadState["phase"];
  error: string | null;
  found: boolean | null;
  reread: boolean;
  blocker: Blocker | null;
  expected: SessionLoadingStatus | null;
};

type Cases = {
  statuses: StatusRow[];
  operational: StatusRow[];
  matches: {
    sameSession: boolean;
    selectedNode: string | null;
    targetNode: string | null;
    expected: boolean;
  }[];
  rereadBudget: number;
  rereads: { used: number; armed: boolean }[];
};

const run = promisify(execFile);
const SELECTED = "session-selected";
const OTHER = "session-other";
const NODE = "did:key:node-0";

function operational(blocker: Blocker | null): DeploymentOperationalState | null {
  if (!blocker) return null;
  const admissionBlocker: OperationalStatus = {
    kind: blocker.kind,
    layer: blocker.layer,
    reason: "modeled",
    label: blocker.label,
    shortLabel: blocker.label,
    detail: blocker.detail,
    action: blocker.configureInference ? "configureInference" : null,
    animated: false,
  };
  return { admissionBlocker } as unknown as DeploymentOperationalState;
}

/* the modeled match facts become real ids, so the production match runs */
function project(row: StatusRow) {
  return projectSessionLoadingStatus({
    selectedSessionId: row.selected ? SELECTED : null,
    selectedNodeDid: NODE,
    session: { sessionId: row.sessionMatches ? SELECTED : OTHER, nodeDid: NODE },
    sessionLoad: {
      phase: row.phase,
      sessionId: row.loadMatches ? SELECTED : OTHER,
      nodeDid: NODE,
      found: row.found,
      error: row.error,
      reread: row.reread,
    },
    operationalState: operational(row.blocker),
  });
}

describe("generated selected-session load status", () => {
  it("drives the production projection, match and re-read budget with every Lean row", async () => {
    const proofsDir = join(
      dirname(fileURLToPath(import.meta.url)),
      "../../../../crates/gents/proofs",
    );
    await run("lake", ["build", "Proofs.Conformance.ClientSessionLoad"], {
      cwd: proofsDir,
      timeout: 240_000,
    });
    const { stdout } = await run(
      "lake",
      ["env", "lean", "--run", "Proofs/Conformance/ClientSessionLoad.lean"],
      {
        cwd: proofsDir,
        encoding: "utf8",
        timeout: 240_000,
        maxBuffer: 64 * 1024 * 1024,
      },
    );
    const cases = JSON.parse(stdout.trim()) as Cases;

    expect(cases.statuses).toHaveLength(1536);
    expect(cases.operational).toHaveLength(80);
    for (const row of [...cases.statuses, ...cases.operational]) {
      expect(project(row), JSON.stringify(row)).toEqual(row.expected);
    }

    expect(cases.matches).toHaveLength(18);
    for (const row of cases.matches) {
      expect(
        selectionMatches(SELECTED, row.selectedNode, {
          sessionId: row.sameSession ? SELECTED : OTHER,
          nodeDid: row.targetNode,
        }),
      ).toBe(row.expected);
    }

    expect(cases.rereads).toHaveLength(cases.rereadBudget + 2);
    for (const row of cases.rereads) {
      expect(waitingRereadDelay(row.used) !== null).toBe(row.armed);
    }
  }, 300_000);
});
