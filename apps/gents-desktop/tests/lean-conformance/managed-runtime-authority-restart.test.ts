import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";

import {
  authorityRefusalText,
  authorityRestartPlan,
  type AuthorityRestartObservation,
  type AuthorityRestartPlan,
} from "../../src/ui/lib/managedRuntimeAuthority";

type Case = AuthorityRestartObservation & { plan: AuthorityRestartPlan };
const run = promisify(execFile);

it("derives the access-change restart plan from the Lean AuthorityRestart owner", async () => {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.ClientAuthorityRestart"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "Proofs/Conformance/ClientAuthorityRestart.lean"],
    { cwd, timeout: 240_000 },
  );
  const cases = JSON.parse(stdout) as Case[];
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
