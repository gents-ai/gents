import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

import { keepsSeen, sessionShownOn } from "../../src/hooks/sessionShown";

type Input = {
  visible: boolean;
  wide: boolean;
  page: "session" | "other";
  listed: boolean;
};

const CONTRACT_JSON_BEGIN = "---BEGIN GENTS LEAN CONTRACT JSON---";
const CONTRACT_JSON_END = "---END GENTS LEAN CONTRACT JSON---";

const run = promisify(execFile);

/** The Lean family's cases, from the contract snapshot. */
async function leanFamilyCases(
  path: string,
): Promise<{ input: Input; expected: boolean }[]> {
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
    families: Record<string, { cases: { input: Input; expected: boolean }[] }>;
  };
  return snapshot.families[key]!.cases;
}

describe("generated session shown", () => {
  it("drives sessionShownOn and keepsSeen with every Lean case", async () => {
    const cases = await leanFamilyCases(
      "families::session_shown_contracts_seen::cases()",
    );
    expect(cases).toHaveLength(16);
    for (const { input, expected } of cases) {
      /* the session's own page, or the sessions list as any other page */
      const shown = sessionShownOn(
        input.wide,
        input.page === "session" ? "session" : "sessions",
      );
      expect(keepsSeen(input.visible, shown, input.listed), JSON.stringify(input)).toBe(
        expected,
      );
    }
  }, 600_000);
});
