import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";

import {
  interruptTarget,
  interruptVisible,
  isStopping,
} from "../../src/ui/screens/activity-status";

const phases = {
  ready: true,
  submittingRequest: true,
  awaitingObservation: true,
  turnInProgress: true,
  blocked: true,
} satisfies Record<Parameters<typeof interruptVisible>[0], true>;

const run = promisify(execFile);
const id = (value: number | null) => (value == null ? null : String(value));

type Selection = {
  phase: Parameters<typeof interruptVisible>[0];
  tracked: number | null;
  latest: number | null;
  turn: string | null;
  visible: boolean;
  target: number | null;
};
type Stop = {
  inFlight: boolean;
  request: number | null;
  latest: number | null;
  observed: boolean;
  requested: number | null;
  stopping: boolean;
};

it("binds Stop visibility, target and pending state to the Lean owner", async () => {
  const root = join(dirname(fileURLToPath(import.meta.url)), "../../../..");
  const cwd = join(root, "crates/gents/proofs");
  await run("lake", ["build", "Proofs.Conformance.ClientInterruptControl"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "Proofs/Conformance/ClientInterruptControl.lean"],
    { cwd, timeout: 240_000 },
  );
  const cases = JSON.parse(stdout) as { selection: Selection[]; stopping: Stop[] };
  expect(new Set(cases.selection.map((row) => row.phase))).toEqual(
    new Set(Object.keys(phases)),
  );
  expect(cases.selection).toHaveLength(140);
  expect(cases.stopping).toHaveLength(108);
  for (const row of cases.selection) {
    expect(interruptVisible(row.phase), JSON.stringify(row)).toBe(row.visible);
    expect(
      interruptTarget({
        trackedRequestId: id(row.tracked),
        latestRequestId: id(row.latest),
        turnState: row.turn,
      }),
      JSON.stringify(row),
    ).toBe(id(row.target));
  }
  for (const row of cases.stopping) {
    expect(
      isStopping({
        inFlight: row.inFlight,
        requestId: id(row.request),
        latestRequestId: id(row.latest),
        interruptObserved: row.observed,
        requestedStop: id(row.requested),
      }),
      JSON.stringify(row),
    ).toBe(row.stopping);
  }
}, 300_000);
