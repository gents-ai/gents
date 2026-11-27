import { expect, it } from "vitest";

import {
  interruptTarget,
  interruptVisible,
  isStopping,
} from "../../src/ui/screens/activity-status";
import { runLeanGeneratorJson } from "./leanGenerator";

const phases = {
  ready: true,
  submittingRequest: true,
  awaitingObservation: true,
  turnInProgress: true,
  blocked: true,
} satisfies Record<Parameters<typeof interruptVisible>[0], true>;

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
  const cases = await runLeanGeneratorJson<{
    selection: Selection[];
    stopping: Stop[];
  }>(
    "Proofs.Conformance.ClientInterruptControl:olean",
    "Proofs/Conformance/ClientInterruptControl.lean",
  );
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
