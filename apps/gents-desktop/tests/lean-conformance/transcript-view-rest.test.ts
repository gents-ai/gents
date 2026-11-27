import { expect, it } from "vitest";

import { viewRests, type ViewMotion } from "../../src/ui/lib/viewRest";
import { runLeanGeneratorJson } from "./leanGenerator";

it("rests the transcript view exactly where the Lean owner does", async () => {
  const cases = await runLeanGeneratorJson<(ViewMotion & { rests: boolean })[]>(
    "Proofs.Conformance.ClientViewRest:olean",
    "Proofs/Conformance/ClientViewRest.lean",
  );
  expect(cases).toHaveLength(80);
  expect(cases.some((row) => row.rests)).toBe(true);
  for (const { rests, ...motion } of cases)
    expect(viewRests(motion), JSON.stringify(motion)).toBe(rests);
}, 300_000);
