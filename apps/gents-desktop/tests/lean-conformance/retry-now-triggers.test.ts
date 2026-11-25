import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

import { asksRetryNow, type AppEvent } from "../../src/hooks/clientObservation";

type LeanEvent = "shown_again" | "hidden" | "focused" | "online" | "network_changed";

const CONTRACT_JSON_BEGIN = "---BEGIN GENTS LEAN CONTRACT JSON---";
const CONTRACT_JSON_END = "---END GENTS LEAN CONTRACT JSON---";

const run = promisify(execFile);

/** The Lean family's cases, from the contract snapshot. */
async function leanFamilyCases(
  path: string,
): Promise<{ input: { app_event: LeanEvent }; expected: boolean }[]> {
  const key = /^families::(\w+)::cases\(\)$/.exec(path)![1]!;
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
  const snapshot = JSON.parse(stdout.slice(begin, end)) as {
    families: Record<
      string,
      { cases: { input: { app_event: LeanEvent }; expected: boolean }[] }
    >;
  };
  return snapshot.families[key]!.cases;
}

const desktopEvent: Record<LeanEvent, AppEvent> = {
  shown_again: "shown-again",
  hidden: "hidden",
  focused: "focused",
  online: "online",
  network_changed: "network-changed",
};

describe("generated retry-now triggers", () => {
  it("drives asksRetryNow with every Lean app event", async () => {
    const cases = await leanFamilyCases(
      "families::observer_resync_asks_retry_now::cases()",
    );
    expect(cases).toHaveLength(5);
    for (const { input, expected } of cases)
      expect(asksRetryNow(desktopEvent[input.app_event]), input.app_event).toBe(
        expected,
      );
  }, 600_000);
});
