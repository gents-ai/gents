import { expect, it } from "vitest";

import {
  authorityRefusalText,
  authorityRestartPlan,
  type AuthorityRestartObservation,
  type AuthorityRestartPlan,
} from "../../src/ui/lib/managedRuntimeAuthority";
import { runLeanGeneratorJson } from "./leanGenerator";

type Case = AuthorityRestartObservation & { plan: AuthorityRestartPlan };

it("derives the access-change restart plan from the Lean AuthorityRestart owner", async () => {
  const cases = await runLeanGeneratorJson<Case[]>(
    "Proofs.Conformance.ClientAuthorityRestart:olean",
    "Proofs/Conformance/ClientAuthorityRestart.lean",
  );
  // The model enumerates every observation: three independent booleans.
  expect(cases).toHaveLength(2 ** 3);
  for (const { plan, ...observation } of cases) {
    const derived = authorityRestartPlan(observation);
    expect(derived, JSON.stringify(observation)).toEqual(plan);
    if (derived.kind !== "refused") continue;
    const text = authorityRefusalText(derived);
    expect(text).toMatch(/keep the current folder/);
    if (derived.nextActions.includes("retryWhenLoaded"))
      expect(text).toMatch(/agents to appear/);
    if (derived.nextActions.includes("addAgent")) expect(text).toMatch(/add an agent/);
  }
}, 300_000);
