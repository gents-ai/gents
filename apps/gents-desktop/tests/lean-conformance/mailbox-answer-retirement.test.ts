import { expect, it } from "vitest";
import { mayRetireMailboxAnswer } from "../../src/hooks/mailboxAnswerRetirement";
import { runLeanGeneratorJson } from "./leanGenerator";

type Observation = NonNullable<Parameters<typeof mayRetireMailboxAnswer>[1]>;
type Case = {
  name: string;
  binding: Parameters<typeof mayRetireMailboxAnswer>[0];
  observation:
    (Observation & { item: Observation["item"] & { status: string } }) | null;
  currentClient: string;
  retire: boolean;
};

it("derives answer-cache retirement from the executed Mailbox owner", async () => {
  const { statuses, cases } = await runLeanGeneratorJson<{
    statuses: { status: string; terminal: boolean }[];
    cases: Case[];
  }>(
    "Proofs.Conformance.MailboxAnswerCacheConformance:olean",
    "Proofs/Conformance/MailboxAnswerCacheConformance.lean",
  );
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
