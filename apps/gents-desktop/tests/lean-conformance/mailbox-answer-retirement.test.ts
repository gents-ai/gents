import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";
import { mayRetireMailboxAnswer } from "../../src/hooks/mailboxAnswerRetirement";

type Observation = NonNullable<Parameters<typeof mayRetireMailboxAnswer>[1]>;
type Case = {
  name: string;
  binding: Parameters<typeof mayRetireMailboxAnswer>[0];
  observation:
    (Observation & { item: Observation["item"] & { status: string } }) | null;
  currentClient: string;
  retire: boolean;
};
const run = promisify(execFile);
it("derives answer-cache retirement from the executed Mailbox owner", async () => {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.MailboxReply"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "MailboxAnswerCacheConformance.lean"],
    { cwd, timeout: 240_000 },
  );
  const { statuses, cases } = JSON.parse(stdout) as {
    statuses: { status: string; terminal: boolean }[];
    cases: Case[];
  };
  // The status vocabulary is the model's enumeration, not the case list, so a
  // lost status row (such as `expired`) fails here as well as in the Lean build.
  expect(statuses.length).toBeGreaterThan(0);
  for (const { status, terminal } of statuses)
    expect(
      cases.some(
        (row) => row.observation?.item.status === status && row.retire === terminal,
      ),
      status,
    ).toBe(true);
  for (const row of cases)
    expect(
      mayRetireMailboxAnswer(row.binding, row.observation, row.currentClient),
      row.name,
    ).toBe(row.retire);
}, 300_000);
