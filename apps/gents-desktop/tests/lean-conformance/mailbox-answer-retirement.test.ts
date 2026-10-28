import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";
import { mayRetireMailboxAnswer } from "../../src/hooks/mailboxAnswerRetirement";

type Case = {
  name: string;
  binding: Parameters<typeof mayRetireMailboxAnswer>[0];
  observation: Parameters<typeof mayRetireMailboxAnswer>[1];
  currentClient: string;
  acknowledged: boolean;
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
  const cases = JSON.parse(stdout) as Case[];
  expect(cases).toHaveLength(17);
  expect(cases.filter((row) => row.retire).map((row) => row.name)).toEqual([
    "acted",
    "dismissed",
    "expired",
  ]);
  for (const row of cases)
    expect(
      mayRetireMailboxAnswer(
        row.binding,
        row.observation,
        row.currentClient,
        row.acknowledged,
      ),
      row.name,
    ).toBe(row.retire);
}, 300_000);
