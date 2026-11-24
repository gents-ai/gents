import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";

import { viewRests, type ViewMotion } from "../../src/ui/lib/viewRest";

const run = promisify(execFile);

it("rests the transcript view exactly where the Lean owner does", async () => {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.ClientViewRest"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "Proofs/Conformance/ClientViewRest.lean"],
    { cwd, timeout: 240_000 },
  );
  const cases = JSON.parse(stdout) as (ViewMotion & { rests: boolean })[];
  expect(cases).toHaveLength(80);
  expect(cases.some((row) => row.rests)).toBe(true);
  for (const { rests, ...motion } of cases)
    expect(viewRests(motion), JSON.stringify(motion)).toBe(rests);
}, 300_000);
